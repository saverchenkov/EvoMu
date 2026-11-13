//! Campaigns, Milestones, Community Tracks, and Telemetry RPC Service Handlers.
//!
//! Implements the V24 client RPC surface for:
//! - `loginCampaignManager.get_client_campaign_state` and `loginCampaignManager.claim_reward`
//!   (backed by `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`)
//! - `seasonalLoginCampaignManager.get_active_campaign`, `seasonalLoginCampaignManager.get_claim_history`,
//!   and `seasonalLoginCampaignManager.claim_reward`
//!   (backed by `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`)
//! - `infoGatheringMgr.GetStateAndConfig` and `infoGatheringMgr.LogInfoEventsFromClient`
//! - `milestoneMgr.ProcessCharacterLogon` and `milestoneMgr.ClaimRewards`
//!   (backed by `res:/staticdata/community_track_templates.fsdbinary` and `seasons` FSD tables)
//! - `communityTrackMgr.get_instance_attributes_public` and `communityTrackMgr.change_qa_mode`
//!   (backed by `res:/staticdata/community_track_templates.fsdbinary`)

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use crate::router::ServerContext;
use crate::services::cosmetics_ui_ops::{load_cosmetics_ui_state, save_cosmetics_ui_state};

#[path = "campaigns_milestones_playtime.rs"]
pub mod playtime;

/// FIFO buffer capacity for client telemetry events.
pub const MAX_TELEMETRY_EVENTS: usize = 1_000;

/// Proleptic Gregorian calendar date ordinal for Unix Epoch (1970-01-01).
/// V24 client `seasonalLoginCampaignService.pyj` uses `datetime.date.fromordinal(k)`.
pub const GREGORIAN_EPOCH_ORDINAL: i64 = 719_163;

/// Default daily login campaign ID from `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`
/// (campaign `2261`: Capsuleer Day XXI, 30-day track with Alpha and Omega item rewards).
pub const DEFAULT_LOGIN_CAMPAIGN_ID: i64 = 2261;

/// Default seasonal login campaign ID from `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`
/// (campaign `2323`: Capsuleer Day XXII, 30-day track with Alpha and Omega item rewards).
pub const DEFAULT_SEASONAL_CAMPAIGN_ID: i64 = 2323;

/// Reward entry decoded from `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdeLoginRewardEntry {
    pub type_id: u32,
    pub quantity: u32,
    pub tier: u32,
    pub blueprint_runs: Option<u32>,
    pub blueprint_material_level: Option<u32>,
    pub blueprint_productivity_level: Option<u32>,
    pub redeemed_expiry_days: Option<u32>,
    pub is_auto_injected_booster_or_skin: bool,
    pub overlay_texture_path: Option<String>,
}

/// Campaign definition decoded from `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdeLoginCampaign {
    pub campaign_id: i64,
    pub campaign_name_id: u32,
    pub auto_claim: bool,
    pub retro_claim: bool,
    pub window_header_background: Option<String>,
    pub alpha_rewards: BTreeMap<u32, SdeLoginRewardEntry>,
    pub omega_rewards: BTreeMap<u32, SdeLoginRewardEntry>,
}

impl SdeLoginCampaign {
    /// Total reward track length derived from the SDE campaign's reward tables.
    pub fn track_length(&self) -> i64 {
        self.alpha_rewards.len().max(self.omega_rewards.len()) as i64
    }
}

/// Community track template decoded from `res:/staticdata/community_track_templates.fsdbinary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdeCommunityTrackTemplate {
    pub template_id: i64,
    pub name: String,
    pub goal: u32,
    pub tale_templates: Vec<u32>,
}

static SDE_LOGIN_CAMPAIGNS: OnceLock<BTreeMap<i64, SdeLoginCampaign>> = OnceLock::new();
static SDE_COMMUNITY_TRACKS: OnceLock<BTreeMap<i64, SdeCommunityTrackTemplate>> = OnceLock::new();

fn parse_reward_map(val: Option<&serde_json::Value>) -> BTreeMap<u32, SdeLoginRewardEntry> {
    let mut out = BTreeMap::new();
    let Some(obj) = val.and_then(|v| v.as_object()) else {
        return out;
    };
    for (day_str, r) in obj {
        let Ok(day_idx) = day_str.parse::<u32>() else {
            continue;
        };
        let Some(type_id) = r.get("typeID").and_then(|v| v.as_u64()).map(|v| v as u32) else {
            continue;
        };
        let quantity = r
            .get("quantity")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(1)
            .max(1);
        let tier = r
            .get("tier")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(0);
        out.insert(
            day_idx,
            SdeLoginRewardEntry {
                type_id,
                quantity,
                tier,
                blueprint_runs: r.get("blueprintRuns").and_then(|v| v.as_u64()).map(|v| v as u32),
                blueprint_material_level: r
                    .get("blueprintMaterialLevel")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32),
                blueprint_productivity_level: r
                    .get("blueprintProductivityLevel")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32),
                redeemed_expiry_days: r
                    .get("redeemedExpiryDays")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32),
                is_auto_injected_booster_or_skin: r
                    .get("isAutoInjectedBoosterOrSkin")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    != 0,
                overlay_texture_path: r
                    .get("overlayTexturePath")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            },
        );
    }
    out
}

pub fn get_sde_login_campaigns(sde: &eve_sde::SdeRegistry) -> &'static BTreeMap<i64, SdeLoginCampaign> {
    SDE_LOGIN_CAMPAIGNS.get_or_init(|| {
        let mut map = BTreeMap::new();
        if let Ok(raw) = sde.load_cfsd_nested("res:/staticdata/seasonalcampaignsloginrewards.fsdbinary") {
            for (cid_str, val) in raw {
                let Ok(campaign_id) = cid_str.parse::<i64>() else {
                    continue;
                };
                let campaign_name_id = val
                    .get("campaignName")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .unwrap_or(0);
                let auto_claim = val.get("autoClaim").and_then(|v| v.as_u64()).unwrap_or(0) != 0;
                let retro_claim = val.get("retroClaim").and_then(|v| v.as_u64()).unwrap_or(0) != 0;
                let window_header_background = val
                    .get("windowHeaderBackground")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let alpha_rewards = parse_reward_map(val.get("alphaRewards"));
                let omega_rewards = parse_reward_map(val.get("omegaRewards"));
                map.insert(
                    campaign_id,
                    SdeLoginCampaign {
                        campaign_id,
                        campaign_name_id,
                        auto_claim,
                        retro_claim,
                        window_header_background,
                        alpha_rewards,
                        omega_rewards,
                    },
                );
            }
        }
        map
    })
}

pub fn get_sde_community_tracks(
    sde: &eve_sde::SdeRegistry,
) -> &'static BTreeMap<i64, SdeCommunityTrackTemplate> {
    SDE_COMMUNITY_TRACKS.get_or_init(|| {
        let mut map = BTreeMap::new();
        if let Ok(raw) = sde.load_cfsd_nested("res:/staticdata/community_track_templates.fsdbinary") {
            for (tid_str, val) in raw {
                let Ok(template_id) = tid_str.parse::<i64>() else {
                    continue;
                };
                let name = val
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let goal = val
                    .get("goal")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .unwrap_or(100);
                let tale_templates = val
                    .get("tale_templates")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_u64().map(|n| n as u32))
                            .collect()
                    })
                    .unwrap_or_default();
                map.insert(
                    template_id,
                    SdeCommunityTrackTemplate {
                        template_id,
                        name,
                        goal,
                        tale_templates,
                    },
                );
            }
        }
        map
    })
}

/// Default daily reward turnover time in seconds past 00:00:00 UTC (11:00:00 EVE downtime).
pub const DEFAULT_SEASONAL_TURNOVER_SECS: u32 = 11 * 3_600;

/// Formats daily reward turnover seconds-from-midnight as `%H:%M:%S` per V24 `loginrewards/common/const.pyj` (`TIME_STRING_FORMAT`).
pub fn format_turnover_hms(turnover_secs: u32) -> String {
    let s = turnover_secs % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3_600, (s % 3_600) / 60, s % 60)
}

/// Parses a `%H:%M:%S` daily turnover string into seconds past 00:00:00 UTC.
pub fn parse_turnover_hms(hms: &str) -> Option<u32> {
    let parts: Vec<&str> = hms.trim().split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let hh: u32 = parts[0].parse().ok()?;
    let mm: u32 = parts[1].parse().ok()?;
    let ss: u32 = parts[2].parse().ok()?;
    if hh >= 24 || mm >= 60 || ss >= 60 {
        return None;
    }
    Some(hh * 3_600 + mm * 60 + ss)
}

/// Computes the V24 `TimeKeeper.get_reward_date_from_timestamp` proleptic Gregorian date ordinal.
pub fn seasonal_reward_date_ordinal(unix: u64, turnover_secs: u32) -> i64 {
    let adj = (unix as i64) - i64::from(turnover_secs % 86_400);
    GREGORIAN_EPOCH_ORDINAL + adj.div_euclid(86_400)
}

/// Formats a Unix timestamp as `YYYY-MM-DD HH:MM:SS` UTC (proleptic Gregorian, civil-from-days).
pub fn format_unix_utc(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
        client_error: None,
    }
}

pub(crate) struct DeliveredLoginReward {
    pub(crate) primary_id: i64,
    pub(crate) spawned_item: Option<eve_db::InventoryItemRecord>,
    pub(crate) expire_date_time: Option<i64>,
}

pub(crate) fn deliver_login_reward(
    snap: &mut eve_db::WorldStateSnapshot,
    uid: u64,
    char_id: i64,
    docked_station_id: Option<i64>,
    campaign_id: i64,
    now_unix: u64,
    rw: &SdeLoginRewardEntry,
) -> DeliveredLoginReward {
    let expire_date_time = rw.redeemed_expiry_days.filter(|&d| d > 0).map(|days| {
        let exp_unix = now_unix.saturating_add(u64::from(days) * 86_400);
        ((exp_unix as i64) + 11_644_473_600) * 10_000_000
    });
    if rw.is_auto_injected_booster_or_skin {
        snap.seasons_and_campaigns
            .auto_injected_rewards
            .entry(char_id)
            .or_default()
            .push(rw.type_id);
    }
    if let Some(station_id) = docked_station_id {
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        let record = eve_db::InventoryItemRecord {
            item_id,
            type_id: rw.type_id,
            owner_id: char_id,
            location_id: station_id,
            flag: eve_db::InventoryFlag::Hangar,
            quantity: rw.quantity.max(1) as i32,
            singleton: rw.blueprint_runs.is_some(),
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: rw.blueprint_runs.is_some(),
            blueprint_runs_remaining: rw.blueprint_runs.map(|r| r as i32),
        };
        if let Some(exp_ft) = expire_date_time {
            snap.seasons_and_campaigns
                .login_reward_expiry
                .insert(item_id, exp_ft);
        }
        snap.items.insert(item_id, record.clone());
        DeliveredLoginReward {
            primary_id: item_id,
            spawned_item: Some(record),
            expire_date_time,
        }
    } else {
        let token_id = crate::services::account_services::enqueue_redeem_token_in_snapshot(
            snap,
            uid,
            rw.type_id,
            rw.quantity.max(1) as i32,
            format!("Campaign {campaign_id} login reward"),
            None,
            expire_date_time,
        );
        DeliveredLoginReward {
            primary_id: token_id,
            spawned_item: None,
            expire_date_time,
        }
    }
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("loginCampaignManager", "get_client_campaign_state") => {
            Some(handle_login_campaign_get_state(ctx, session))
        }
        ("loginCampaignManager", "claim_reward") => {
            Some(handle_login_campaign_claim_reward(ctx, session))
        }
        // parity: rpc:seasonalLoginCampaignManager.get_active_campaign
        ("seasonalLoginCampaignManager", "get_active_campaign") => {
            Some(handle_seasonal_login_get_active_campaign(ctx, session))
        }
        // parity: rpc:seasonalLoginCampaignManager.get_claim_history
        ("seasonalLoginCampaignManager", "get_claim_history") => {
            Some(handle_seasonal_login_get_claim_history(ctx, session))
        }
        // parity: rpc:seasonalLoginCampaignManager.claim_reward
        ("seasonalLoginCampaignManager", "claim_reward") => {
            Some(handle_seasonal_login_claim_reward(ctx, session))
        }
        ("infoGatheringMgr", "GetStateAndConfig") => {
            Some(handle_info_gathering_get_state_and_config(ctx, session))
        }
        ("infoGatheringMgr", "LogInfoEventsFromClient") => {
            Some(handle_info_gathering_log_events(ctx, session, args))
        }
        ("milestoneMgr", "ProcessCharacterLogon") => {
            Some(handle_milestone_process_logon(ctx, session))
        }
        ("milestoneMgr", "ClaimRewards") => {
            Some(handle_milestone_claim_rewards(ctx, session, args))
        }
        ("communityTrackMgr", "get_instance_attributes_public") => {
            Some(handle_community_track_get_attributes(ctx, session, args))
        }
        // parity: rpc:communityTrackMgr.change_qa_mode
        ("communityTrackMgr", "change_qa_mode") => {
            Some(handle_community_track_change_qa_mode(ctx, session, args))
        }
        _ => None,
    }
}

/// Returns the current EVE downtime day index (turnover at 11:00 UTC = 39,600s past midnight).
pub fn eve_downtime_day_bucket(now_unix: u64) -> u64 {
    let downtime_offset_secs: u64 = 11 * 3600;
    if now_unix >= downtime_offset_secs {
        (now_unix - downtime_offset_secs) / 86_400
    } else {
        now_unix / 86_400
    }
}

fn handle_login_campaign_get_state(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;

    // Character-selection screen before character selected: no campaign state
    let Some(char_id) = session.character_id else {
        return Ok(PyRep::None);
    };

    let now = ctx.now_unix();
    let today = eve_downtime_day_bucket(now);
    let campaigns = get_sde_login_campaigns(&ctx.sde);

    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let (campaign_id, is_rookie, campaign) =
            playtime::resolve_daily_campaign(snap, char_id, now, campaigns)?;
        let track_length = campaign.track_length();

        let claimed_days = ext
            .login_campaign_claimed_days
            .get(&(char_id, campaign_id))
            .cloned()
            .unwrap_or_default();
        let already_claimed_today = claimed_days.contains(&today)
            || ext.last_login_reward_claim_day_by_char.get(&char_id).copied() == Some(today);
        let num_claimed = claimed_days.len() as i64;
        let next_reward_idx = num_claimed + 1;

        let claimed_reps: Vec<PyRep> = (1..=num_claimed).map(PyRep::Integer).collect();
        let next_reward_timestamp = if already_claimed_today {
            let next_downtime_unix = (today + 1) * 86_400 + 11 * 3_600;
            ((next_downtime_unix as i64) + 11_644_473_600) * 10_000_000
        } else {
            0
        };

        let mut rewards_by_day = Vec::with_capacity(campaign.alpha_rewards.len());
        for (&day_idx, rw) in &campaign.alpha_rewards {
            let day_1 = i64::from(day_idx) + 1;
            let label_msg_id =
                playtime::resolve_reward_label_message_id(ctx, rw.type_id, campaign);
            let mut entries = vec![
                ("typeID", PyRep::Integer(i64::from(rw.type_id))),
                ("quantity", PyRep::Integer(i64::from(rw.quantity))),
                ("tier", PyRep::Integer(i64::from(rw.tier))),
                ("labelMessageID", PyRep::Integer(label_msg_id)),
            ];
            if let Some(ref overlay) = rw.overlay_texture_path {
                entries.push(("overlayTexturePath", PyRep::String(overlay.clone())));
            }
            if let Some(mat) = rw.blueprint_material_level {
                entries.push(("blueprintMaterialLevel", PyRep::Integer(i64::from(mat))));
            }
            if let Some(prod) = rw.blueprint_productivity_level {
                entries.push(("blueprintProductivityLevel", PyRep::Integer(i64::from(prod))));
            }
            rewards_by_day.push((PyRep::Integer(day_1), PyRep::keyval(entries)));
        }

        let header_path = campaign
            .window_header_background
            .clone()
            .unwrap_or_else(|| "res:/UI/Texture/Classes/LoginCampaign/header_narrow.png".into());
        let static_data = PyRep::keyval([
            ("campaign_id", PyRep::Integer(campaign_id)),
            (
                "titleMessageID",
                PyRep::Integer(i64::from(campaign.campaign_name_id)),
            ),
            (
                "subtitleMessageID",
                PyRep::Integer(i64::from(campaign.campaign_name_id)),
            ),
            (
                "windowHeaderBackgroundNarrow",
                PyRep::String(header_path.clone()),
            ),
            ("windowHeaderBackgroundWide", PyRep::String(header_path)),
            ("campaign_duration", PyRep::Integer(track_length)),
            ("is_rookie_campaign", PyRep::Bool(is_rookie)),
            ("item_rewards_by_day", PyRep::Dict(rewards_by_day)),
        ]);

        let item_progress = PyRep::keyval([
            ("next_reward_index", PyRep::Integer(next_reward_idx)),
            ("num_claimed_rewards", PyRep::Integer(num_claimed)),
            ("reward_count", PyRep::Integer(track_length)),
            ("claimed_rewards", PyRep::List(claimed_reps)),
            ("next_reward_timestamp", PyRep::Integer(next_reward_timestamp)),
        ]);

        let bucket_progress = PyRep::keyval([
            ("progress", PyRep::Integer(num_claimed)),
            ("total", PyRep::Integer(track_length)),
        ]);

        Ok(PyRep::keyval([
            ("static_data", static_data),
            ("item_progress", item_progress),
            ("bucket_progress", bucket_progress),
            ("reward_event", PyRep::None),
        ]))
    })
}

fn handle_login_campaign_claim_reward(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (uid, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    let today = eve_downtime_day_bucket(now);
    let campaigns = get_sde_login_campaigns(&ctx.sde);
    let sess_docked = session.station_id.or(session.structure_id);

    let (res, spawned_items) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let (campaign_id, _is_rookie, campaign) =
            playtime::resolve_daily_campaign(snap, char_id, now, campaigns)?;
        let track_length = campaign.track_length();

        let (docked_station_id, clone_grade) = snap
            .characters
            .get(&char_id)
            .map(|c| {
                let stn = playtime::resolve_reward_station_id(sess_docked, c);
                (stn, c.clone_grade)
            })
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;

        if ext.last_login_reward_claim_day_by_char.get(&char_id).copied() == Some(today) {
            return Err(err(
                "DailyRewardAlreadyClaimedToday",
                "Daily login reward already claimed today",
            ));
        }

        let claimed_days = ext
            .login_campaign_claimed_days
            .entry((char_id, campaign_id))
            .or_default();
        if claimed_days.contains(&today) {
            return Err(err(
                "DailyRewardAlreadyClaimedToday",
                "Daily login reward already claimed today",
            ));
        }
        let day_idx = claimed_days.len() as u32;
        if i64::from(day_idx) >= track_length {
            return Err(err(
                "CampaignCompleted",
                format!("All {track_length} daily rewards in campaign {campaign_id} have been claimed"),
            ));
        }

        let alpha_rw = campaign.alpha_rewards.get(&day_idx);
        let omega_rw = if clone_grade >= 2 {
            campaign.omega_rewards.get(&day_idx)
        } else {
            None
        };
        let primary_rw = alpha_rw.or(omega_rw).ok_or_else(|| {
            err(
                "RewardDefinitionMissing",
                format!("No SDE reward entry for campaign {campaign_id} day index {day_idx}"),
            )
        })?;

        let mut delivered_item_ids = Vec::new();
        let mut spawned_items = Vec::new();
        let mut primary_expire_date_time = None;
        for rw in [alpha_rw, omega_rw].into_iter().flatten() {
            let del = deliver_login_reward(
                snap,
                uid,
                char_id,
                docked_station_id,
                campaign_id,
                now,
                rw,
            );
            delivered_item_ids.push(del.primary_id);
            if primary_expire_date_time.is_none() {
                primary_expire_date_time = del.expire_date_time;
            }
            if let Some(rec) = del.spawned_item {
                spawned_items.push(rec);
            }
        }

        claimed_days.push(today);
        let reward_index = claimed_days.len() as i64;
        ext.last_login_reward_claim_day_by_char.insert(char_id, today);
        let claimed_reps: Vec<PyRep> = (1..=reward_index).map(PyRep::Integer).collect();
        snap.next_entity_id += 1;
        let claim_event_id = snap.next_entity_id;
        save_cosmetics_ui_state(snap, &ext);

        let next_downtime_unix = (today + 1) * 86_400 + 11 * 3_600;
        let next_reward_timestamp = ((next_downtime_unix as i64) + 11_644_473_600) * 10_000_000;

        let reward_item = PyRep::keyval([
            ("event_id", PyRep::Integer(claim_event_id)),
            ("campaign_id", PyRep::Integer(campaign_id)),
            ("reward_index", PyRep::Integer(reward_index)),
            ("typeID", PyRep::Integer(i64::from(primary_rw.type_id))),
            ("quantity", PyRep::Integer(i64::from(primary_rw.quantity))),
            ("tier", PyRep::Integer(i64::from(primary_rw.tier))),
            ("itemID", PyRep::Integer(delivered_item_ids[0])),
            (
                "delivered_item_ids",
                PyRep::List(delivered_item_ids.iter().copied().map(PyRep::Integer).collect()),
            ),
            (
                "expireDateTime",
                primary_expire_date_time.map(PyRep::Integer).unwrap_or(PyRep::None),
            ),
        ]);

        let item_progress = PyRep::keyval([
            ("next_reward_index", PyRep::Integer(reward_index + 1)),
            ("num_claimed_rewards", PyRep::Integer(reward_index)),
            ("reward_count", PyRep::Integer(track_length)),
            ("claimed_rewards", PyRep::List(claimed_reps)),
            ("next_reward_timestamp", PyRep::Integer(next_reward_timestamp)),
        ]);

        let bucket_progress = PyRep::keyval([
            ("progress", PyRep::Integer(reward_index)),
            ("total", PyRep::Integer(track_length)),
        ]);

        // V24 `loginCampaignService.pyj`: unpacks 4-tuple `(item_reward, updated_item_progress, received_bucket_typeID, updated_bucket_progress)`
        Ok((
            PyRep::Tuple(vec![
                reward_item,
                item_progress,
                PyRep::None,
                bucket_progress,
            ]),
            spawned_items,
        ))
    })?;
    if res != PyRep::None {
        for item in &spawned_items {
            let (group_id, category_id) = ctx
                .sde
                .types
                .get(&item.type_id)
                .map(|t| (t.group_id, t.category_id))
                .unwrap_or((0, 0));
            let item_change_args = vec![
                PyRep::keyval([
                    ("itemID", PyRep::Integer(item.item_id)),
                    ("typeID", PyRep::Integer(i64::from(item.type_id))),
                    ("ownerID", PyRep::Integer(char_id)),
                    ("locationID", PyRep::Integer(item.location_id)),
                    ("flagID", PyRep::Integer(eve_db::InventoryFlag::Hangar as i64)),
                    (
                        "quantity",
                        PyRep::Integer(if item.singleton {
                            -1
                        } else {
                            i64::from(item.quantity)
                        }),
                    ),
                    ("groupID", PyRep::Integer(i64::from(group_id))),
                    ("categoryID", PyRep::Integer(i64::from(category_id))),
                    ("singleton", PyRep::Bool(item.singleton)),
                    ("stacksize", PyRep::Integer(i64::from(item.quantity))),
                ]),
                PyRep::Dict(Vec::new()),
            ];
            if !session.live_notify_listener {
                session.push_notification("OnItemChange", item_change_args.clone());
            }
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnItemChange",
                "charid",
                item_change_args,
            );
        }
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnRedeemingQueueUpdated",
            "charid",
            vec![],
        );
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnRedeemingTokensUpdated",
            "charid",
            vec![],
        );
    }
    Ok(res)
}

fn handle_seasonal_login_get_active_campaign(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;

    let Some(char_id) = session.character_id else {
        return Ok(PyRep::Tuple(vec![
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::None,
        ]));
    };

    let now = ctx.now_unix();
    let campaigns = get_sde_login_campaigns(&ctx.sde);
    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let Some(sched) = playtime::active_seasonal_schedule_from_snapshot(snap, &ext, now)
        else {
            return Ok(PyRep::Tuple(vec![
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::None,
            ]));
        };
        let campaign = campaigns.get(&sched.campaign_id).ok_or_else(|| {
            err(
                "CampaignNotFound",
                format!(
                    "Seasonal campaign {} not found in SDE seasonalcampaignsloginrewards",
                    sched.campaign_id
                ),
            )
        })?;
        let claimed_count = ext
            .seasonal_claim_history
            .get(&char_id)
            .and_then(|m| m.get(&sched.campaign_id))
            .map_or(0, |v| v.len() as i64);
        let end_date_ordinal = seasonal_reward_date_ordinal(sched.end_unix, sched.turnover_secs);
        let today_ordinal = seasonal_reward_date_ordinal(now, sched.turnover_secs);
        let claimed_today = ext
            .seasonal_claim_history
            .get(&char_id)
            .and_then(|m| m.get(&sched.campaign_id))
            .is_some_and(|v| v.iter().any(|&(ord, _)| ord == today_ordinal));
        let days_remaining = (end_date_ordinal - today_ordinal + i64::from(!claimed_today)).max(0);
        let num_remaining_rewards = (campaign.track_length() - claimed_count).max(0).min(days_remaining);
        let turnover_time_str = format_turnover_hms(sched.turnover_secs);

        Ok(PyRep::Tuple(vec![
            PyRep::Integer(sched.campaign_id),
            PyRep::Integer(end_date_ordinal),
            PyRep::String(turnover_time_str),
            PyRep::Integer(num_remaining_rewards),
        ]))
    })
}

fn handle_seasonal_login_get_claim_history(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let Some(sched) = playtime::active_seasonal_schedule_from_snapshot(snap, &ext, now)
        else {
            return Ok(PyRep::Dict(Vec::new()));
        };
        let entries: Vec<(PyRep, PyRep)> = ext
            .seasonal_claim_history
            .get(&char_id)
            .and_then(|m| m.get(&sched.campaign_id))
            .map(|claims| {
                claims
                    .iter()
                    .map(|&(date_ordinal, client_clone_state)| {
                        // V24 `seasonalLoginCampaignService.pyj`: `{date_ordinal: clone_grade}` where `CLONE_STATE_ALPHA = 0`, `CLONE_STATE_OMEGA = 1`
                        (
                            PyRep::Integer(date_ordinal),
                            PyRep::Integer(i64::from(client_clone_state)),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(PyRep::Dict(entries))
    })
}

fn handle_seasonal_login_claim_reward(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (uid, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    let campaigns = get_sde_login_campaigns(&ctx.sde);
    let sess_docked = session.station_id.or(session.structure_id);

    let (rep, outcome) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let sched = playtime::active_seasonal_schedule_from_snapshot(snap, &ext, now)
            .ok_or_else(|| err("NoActiveCampaign", "No seasonal campaign is currently active"))?;
        let campaign_id = sched.campaign_id;
        let campaign = campaigns.get(&campaign_id).ok_or_else(|| {
            err(
                "CampaignNotFound",
                format!("Seasonal campaign {campaign_id} not found in SDE seasonalcampaignsloginrewards"),
            )
        })?;
        let track_length = campaign.track_length();
        let today_ordinal = seasonal_reward_date_ordinal(now, sched.turnover_secs);

        let (docked_station_id, clone_grade) = snap
            .characters
            .get(&char_id)
            .map(|c| {
                let stn = sess_docked.or(c.station_id).or(c.structure_id);
                (stn, c.clone_grade)
            })
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        // V24 `clonegrade/const.pyj`: `CLONE_STATE_ALPHA = 0`, `CLONE_STATE_OMEGA = 1` (`CharacterRecord.clone_grade >= 2` is Omega)
        let is_omega = clone_grade >= 2;
        let client_clone_state: u8 = u8::from(is_omega);

        let mut retro_indices: Vec<u32> = Vec::new();
        let (already_claimed_today, day_idx) = {
            let claims = ext
                .seasonal_claim_history
                .entry(char_id)
                .or_default()
                .entry(campaign_id)
                .or_default();
            if campaign.retro_claim && is_omega {
                for (idx, (_, state)) in claims.iter().enumerate() {
                    if *state == 0 {
                        retro_indices.push(idx as u32);
                    }
                }
            }
            let already = claims.iter().any(|&(ord, _)| ord == today_ordinal);
            (already, claims.len() as u32)
        };

        if already_claimed_today && retro_indices.is_empty() {
            return Err(err(
                "SeasonalRewardAlreadyClaimedToday",
                "Seasonal reward already claimed for today",
            ));
        }
        if !already_claimed_today && i64::from(day_idx) >= track_length && retro_indices.is_empty() {
            return Err(err(
                "CampaignCompleted",
                format!("All {track_length} seasonal rewards in campaign {campaign_id} have been claimed"),
            ));
        }

        let mut delivered_any = false;
        let mut created_hangar_items: Vec<(i64, u32, i64, u32)> = Vec::new();
        for retro_idx in &retro_indices {
            if let Some(rw) = campaign.omega_rewards.get(retro_idx) {
                let del = deliver_login_reward(snap, uid, char_id, docked_station_id, campaign_id, now, rw);
                if let Some(loc_id) = docked_station_id {
                    created_hangar_items.push((del.primary_id, rw.type_id, loc_id, rw.quantity.max(1)));
                }
                delivered_any = true;
            }
        }

        let mut claimed_new_today = false;
        if !already_claimed_today && i64::from(day_idx) < track_length {
            let alpha_rw = campaign.alpha_rewards.get(&day_idx);
            let omega_rw = is_omega.then(|| campaign.omega_rewards.get(&day_idx)).flatten();
            if alpha_rw.is_none() && omega_rw.is_none() && !delivered_any {
                return Err(err(
                    "RewardDefinitionMissing",
                    format!("No SDE reward entry for seasonal campaign {campaign_id} day index {day_idx}"),
                ));
            }
            if let Some(rw) = alpha_rw {
                let del = deliver_login_reward(snap, uid, char_id, docked_station_id, campaign_id, now, rw);
                if let Some(loc_id) = docked_station_id {
                    created_hangar_items.push((del.primary_id, rw.type_id, loc_id, rw.quantity.max(1)));
                }
                delivered_any = true;
            }
            if let Some(rw) = omega_rw {
                let del = deliver_login_reward(snap, uid, char_id, docked_station_id, campaign_id, now, rw);
                if let Some(loc_id) = docked_station_id {
                    created_hangar_items.push((del.primary_id, rw.type_id, loc_id, rw.quantity.max(1)));
                }
                delivered_any = true;
            }
            claimed_new_today = true;
        }

        {
            let claims = ext
                .seasonal_claim_history
                .entry(char_id)
                .or_default()
                .entry(campaign_id)
                .or_default();
            for &retro_idx in &retro_indices {
                if let Some(entry) = claims.get_mut(retro_idx as usize) {
                    entry.1 = 1;
                }
            }
            if claimed_new_today {
                claims.push((today_ordinal, client_clone_state));
            }
        }
        save_cosmetics_ui_state(snap, &ext);

        Ok((
            PyRep::Bool(true),
            crate::services::operations_manager_helpers::OperationRewardOutcome {
                created_hangar_items,
                enqueued_redeem_tokens: delivered_any && docked_station_id.is_none(),
                ..Default::default()
            },
        ))
    })?;
    crate::services::operations_manager_helpers::emit_operation_reward_notifications(
        ctx,
        char_id,
        &outcome,
    );
    Ok(rep)
}

/// Global config keys (`machoNet.SetGlobalConfigValue`) controlling `infoGatheringMgr`.
pub const INFO_GATHERING_ENABLED_CONFIG_KEY: &str = "infoGatheringEnabled";
pub const INFO_GATHERING_INTERVAL_CONFIG_KEY: &str = "infoGatheringClientWorkerInterval";

fn info_gathering_config(ctx: &ServerContext) -> (bool, i64) {
    let flags = ctx.feature_flags_snapshot();
    let mut enabled = flags.enable_info_gathering;
    let mut interval = (flags.info_gathering_interval_secs as i64).max(1);
    if let Ok(guard) = ctx.global_config_values.read() {
        if let Some(val) = guard.get(INFO_GATHERING_ENABLED_CONFIG_KEY) {
            enabled = match val {
                PyRep::Bool(b) => *b,
                PyRep::Integer(i) => *i != 0,
                PyRep::String(s) => s == "1" || s.eq_ignore_ascii_case("true"),
                _ => enabled,
            };
        }
        if let Some(val) = guard.get(INFO_GATHERING_INTERVAL_CONFIG_KEY) {
            let parsed = match val {
                PyRep::Integer(i) => Some(*i),
                PyRep::String(s) => s.parse::<i64>().ok(),
                _ => None,
            };
            if let Some(v) = parsed {
                interval = v.max(1);
            }
        }
    }
    (enabled, interval)
}

fn handle_info_gathering_get_state_and_config(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;

    let (is_enabled, worker_interval) = info_gathering_config(ctx);

    let mut sde_event_ids: Vec<i64> = ctx
        .sde
        .load_event_types()
        .map(|m| m.into_keys().map(i64::from).collect())
        .unwrap_or_default();
    sde_event_ids.sort_unstable();
    let info_types: Vec<PyRep> = sde_event_ids.into_iter().map(PyRep::Integer).collect();

    Ok(PyRep::keyval([
        ("clientWorkerInterval", PyRep::Integer(worker_interval)),
        ("infoTypeAggregates", PyRep::Dict(Vec::new())),
        ("infoTypeParameters", PyRep::Dict(Vec::new())),
        ("infoTypes", PyRep::List(info_types)),
        ("infoTypesOncePerRun", PyRep::List(Vec::new())),
        ("isEnabled", PyRep::Bool(is_enabled)),
    ]))
}

fn handle_info_gathering_log_events(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;

    let events = args
        .first()
        .ok_or_else(|| err("InvalidArgument", "Missing events dictionary or list"))?;

    let (is_enabled, _) = info_gathering_config(ctx);
    let status_code = if is_enabled { 0 } else { 1 };

    let (status_entries, event_logs): (Vec<(PyRep, PyRep)>, Vec<String>) = match events {
        PyRep::Dict(entries) => {
            let mut status = Vec::with_capacity(entries.len());
            let mut logs = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                let key_str = match k {
                    PyRep::String(s) => s.clone(),
                    PyRep::Integer(i) => i.to_string(),
                    _ => "unknown".to_string(),
                };
                logs.push(format!("{key_str}:{v:?}"));
                status.push((k.clone(), PyRep::Integer(status_code)));
            }
            (status, logs)
        }
        PyRep::List(items) => {
            let mut status = Vec::with_capacity(items.len());
            let mut logs = Vec::with_capacity(items.len());
            for (idx, item) in items.iter().enumerate() {
                let key = match item {
                    PyRep::Integer(id) => PyRep::Integer(*id),
                    PyRep::String(s) => PyRep::String(s.clone()),
                    _ => PyRep::Integer(idx as i64),
                };
                logs.push(format!("{item:?}"));
                status.push((key, PyRep::Integer(status_code)));
            }
            (status, logs)
        }
        _ => {
            return Err(err(
                "InvalidArgument",
                "LogInfoEventsFromClient requires Dict or List of events",
            ))
        }
    };

    if is_enabled {
        // Record in bounded FIFO telemetry buffer in snapshot
        ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_cosmetics_ui_state(snap);
            ext.client_telemetry_events.extend(event_logs);
            if ext.client_telemetry_events.len() > MAX_TELEMETRY_EVENTS {
                let overflow = ext.client_telemetry_events.len() - MAX_TELEMETRY_EVENTS;
                ext.client_telemetry_events.drain(0..overflow);
            }
            save_cosmetics_ui_state(snap, &ext);
        });
    }

    Ok(PyRep::Dict(status_entries))
}

fn handle_milestone_process_logon(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();

    let (already_claimed, remaining_secs) = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.milestone_logons.entry(char_id).or_insert(now);
        let st = playtime::record_milestone_logon(snap, char_id, now);
        let claimed = ext
            .claimed_milestones
            .get(&char_id)
            .is_some_and(|v| v.contains(&playtime::MILESTONE_HUNDRED_AND_TWENTY_MINUTES_ID));
        save_cosmetics_ui_state(snap, &ext);
        let rem = playtime::MILESTONE_REWARD_NUMBER_OF_MINUTES_SECS
            .saturating_sub(st.accumulated_playtime_secs);
        (claimed, rem)
    });

    if !already_claimed {
        let end_ft = ctx
            .now_blueos_filetime_ticks()
            .saturating_add((remaining_secs as i64) * 10_000_000);
        crate::services::notify_emitters_skills_trade_godma::emit_login_milestone_timer_started(
            ctx,
            char_id,
            end_ft,
        );
    }

    Ok(PyRep::None)
}

fn handle_milestone_claim_rewards(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let milestone_id = args
        .first()
        .and_then(|v| v.as_i64())
        .ok_or_else(|| err("InvalidArgument", "ClaimRewards requires milestone_id integer"))?;
    if milestone_id != playtime::MILESTONE_HUNDRED_AND_TWENTY_MINUTES_ID {
        return Err(err(
            "InvalidMilestoneID",
            format!("Unknown milestone_id {milestone_id}"),
        ));
    }

    let now = ctx.now_unix();
    let reward_isk = playtime::MILESTONE_HUNDRED_AND_TWENTY_MINUTES_REWARD_ISK;
    let add_cents: i64 = reward_isk.saturating_mul(100);

    let (bal_after_cents, _) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        if !ext.milestone_logons.contains_key(&char_id) {
            return Err(err(
                "MilestonePrerequisiteNotMet",
                "ProcessCharacterLogon must be called before claiming milestone rewards",
            ));
        }

        let claimed = ext.claimed_milestones.entry(char_id).or_default();
        if claimed.contains(&milestone_id) {
            return Err(err(
                "MilestoneAlreadyClaimed",
                format!("Milestone {milestone_id} has already been claimed"),
            ));
        }

        let mut st = playtime::load_milestone_state(snap, char_id).ok_or_else(|| {
            err(
                "MilestonePrerequisiteNotMet",
                "ProcessCharacterLogon must be called before claiming milestone rewards",
            )
        })?;
        let total_played = st.accumulated_playtime_now(now);
        if total_played < playtime::MILESTONE_REWARD_NUMBER_OF_MINUTES_GRACE_PERIOD_SECS {
            return Err(err(
                "MilestonePrerequisiteNotMet",
                format!(
                    "Milestone {milestone_id} requires {}s accumulated play time (elapsed: {total_played}s)",
                    playtime::MILESTONE_REWARD_NUMBER_OF_MINUTES_GRACE_PERIOD_SECS
                ),
            ));
        }
        st.accumulated_playtime_secs = total_played;
        st.last_activity_unix = now;
        playtime::save_milestone_state(snap, char_id, &st);

        let (bal_after, _) = crate::domain::career_goals::credit_character_reward_in_snap(
            snap,
            char_id,
            eve_db::IskCents(add_cents),
            0.0,
            0,
            Some((
                "MilestoneReward",
                format!("Login Milestone {milestone_id} Achieved (120 Minutes)"),
                None,
            )),
        )
        .map_err(|e| err(&e.code, e.message))?;

        claimed.push(milestone_id);
        save_cosmetics_ui_state(snap, &ext);
        Ok((bal_after.0, add_cents))
    })?;

    playtime::emit_account_change_isk(ctx, session, char_id, bal_after_cents);
    crate::services::notify_emitters_character_services::emit_login_milestone_achieved(
        ctx,
        char_id,
        milestone_id,
        reward_isk as f64,
    );

    Ok(PyRep::None)
}

fn handle_community_track_get_attributes(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;

    let instance_id = args.first().and_then(|v| v.as_i64()).ok_or_else(|| {
        err(
            "InvalidArgument",
            "get_instance_attributes_public requires instance_id integer",
        )
    })?;

    let Ok(track_uid) = u32::try_from(instance_id) else {
        return Err(err(
            "InvalidInstanceID",
            format!("Instance ID {instance_id} is invalid"),
        ));
    };
    if track_uid == 0 {
        return Err(err(
            "InvalidInstanceID",
            format!("Instance ID {instance_id} is invalid"),
        ));
    }

    let templates = get_sde_community_tracks(&ctx.sde);

    let track = ctx
        .db
        .with_snapshot(|snap| {
            snap.quasar_state_blobs
                .get("quasar_community_tracks")
                .and_then(|json| {
                    serde_json::from_str::<
                        HashMap<u32, crate::services::quasar_progression_extra::CommunityTrackState>,
                    >(json)
                    .ok()
                })
                .and_then(|map| map.get(&track_uid).cloned())
        })
        .or_else(|| {
            ctx.quasar_gateway
                .progression
                .extra
                .get_community_track(track_uid)
        })
        .filter(|t| t.active)
        .ok_or_else(|| {
            err(
                "CommunityTrackInstanceNotFound",
                format!("Active community track instance {instance_id} not found"),
            )
        })?;

    let template_id = i64::from(track.template_id);

    let template = templates.get(&template_id).ok_or_else(|| {
        err(
            "CommunityTrackInstanceNotFound",
            format!(
                "Community track instance {instance_id} (template {template_id}) not found in SDE community_track_templates"
            ),
        )
    })?;

    let raw_progress = i64::try_from(track.current_progress).unwrap_or(i64::MAX);
    let progress = raw_progress.clamp(0, i64::from(template.goal));

    // 2-tuple (fsd_identifier, progress) per V24 communitytracks/client/repository.pyj:114.
    Ok(PyRep::Tuple(vec![
        PyRep::Integer(template.template_id),
        PyRep::Integer(progress),
    ]))
}

fn handle_community_track_change_qa_mode(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;

    let has_qa_perm = session.has_gm_role()
        || (session.role
            & (eve_machonet::service_roles::ROLE_QA
                | eve_machonet::service_roles::ROLE_PROGRAMMER))
            != 0;

    if !has_qa_perm {
        return Err(err(
            "AccessDenied",
            "communityTrackMgr.change_qa_mode requires QA or GM privileges",
        ));
    }

    let mode_rep = args
        .first()
        .ok_or_else(|| err("InvalidArgument", "change_qa_mode requires 1 argument (mode)"))?;

    let qa_mode = match mode_rep {
        PyRep::Bool(b) => *b,
        PyRep::Integer(i) => *i != 0,
        _ => return Err(err("InvalidArgument", "mode must be a boolean or integer")),
    };

    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.community_track_qa_mode = qa_mode;
        save_cosmetics_ui_state(snap, &ext);
        snap.service_state_blobs.remove("community_track_qa_mode");
        let _ = snap
            .kv_mut(eve_db::Ns::CommunityTrackQaMode)
            .set_with_unix(0, "", &qa_mode, now_unix);
    });

    Ok(PyRep::Bool(true))
}

#[cfg(test)] #[path = "campaigns_and_milestones_tests.rs"] mod campaigns_and_milestones_tests;

