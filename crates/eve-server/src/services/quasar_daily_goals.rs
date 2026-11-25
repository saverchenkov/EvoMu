//! `quasar_daily_goals` — V24.01 Quasar Daily Goals Service (`feat:quasar.dailygoal`, `feat:pve.daily-goals`, `Q-4`).
//!
//! Implements live backend semantics for the EVE Online V24 AIR Daily Goals system:
//! - Localization message IDs (`titleID`, `progressDescriptionID`, `descriptionID`) seeded from
//!   `res:/staticdata/contributionmethods.fsdbinary` and `res:/localizationfsd/localization_fsd_en-us.pickle`.
//!   Note: V24 AIR Daily Goal targets, rewards, and completion costs are server-authored (`DERIVED`,
//!   not stored in client `.fsdbinary` static files).
//! - Daily goal rotation (at `11:00 UTC` downtime), progress tracking, and reward redemption (ISK, SP).
//! - Request handlers for `eve_public.dailygoal.api` and `eve.dailygoal.api`.
//! - Real-time notice publication (`CurrentGoalsNotice`, `ProgressedNotice`, `CompletedNotice`, `RedeemedNotice`).
//! - Live wallet/skill-point persistence in `WorldRepository`.
//!
//! `WorldStateSnapshot.quasar_state_blobs["quasar_daily_goals"]` persists template overrides and per-character daily goal progress across restarts.

// parity: feat:pve.daily-goals

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_PRECONDITION_FAILED,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{
    DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleTimestamp, WireField, WireMessage,
    WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Operator-configured daily goal template for V24 AIR Daily Operations.
///
/// Contribution categories map to `res:/staticdata/contributionmethods.fsdbinary` and
/// `eve_public.dailygoal.ContributionConfiguration`:
/// - `1`: Kill NPC (`DestroyNonCapsuleers`, titleID `659195`, progressDescriptionID `696251`)
/// - `2`: Mine Ore (`MineMaterials`, titleID `659218`, progressDescriptionID `696247`)
/// - `3`: Scan Signature (`ScanSignature`, titleID `665435`, progressDescriptionID `696245`)
/// - `4`: Space Jump (`JumpStargate`, titleID `541824`, progressDescriptionID `1041584`)
/// - `5`: Manufacture Item (`ManufactureItem`, titleID `665394`, progressDescriptionID `696246`)
/// - `6`: Daily Bonus (`DailyBonus`, titleID `722674`, progressDescriptionID `698433`)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DailyGoalTemplate {
    pub uuid: [u8; 16],
    pub name_message: String,
    pub description_message: String,
    /// Internal contribution method tag (`1`: KillNPC, `2`: MineOre, `3`: ScanSignature,
    /// `4`: SpaceJump, `5`: ManufactureItem, `6`: DailyBonus, `7`: DamageShip).
    pub category: u32,
    /// `eve_public.career.Path` (`0`: Unspecified/General, `1`: Explorer, `2`: Industrialist,
    /// `3`: Enforcer, `4`: Soldier of Fortune).
    pub career: u32,
    pub target: u64,
    pub isk_reward: i64,
    #[serde(default)]
    pub evermarks_reward: i64,
    pub sp_reward: f64,
    #[serde(default)]
    pub completion_cost_isk: Option<i64>,
    #[serde(default)]
    pub completion_cost_evermarks: Option<i64>,
    #[serde(default)]
    pub name_message_id: u32,
    #[serde(default)]
    pub description_message_id: u32,
    #[serde(default)]
    pub help_text_message_id: u32,
    #[serde(default)]
    pub assigner_corporation_id: u32,
}

impl DailyGoalTemplate {
    /// Returns the `.eve_public.dailygoal.Category` (`1` = `CATEGORY_DAILY`, `2` = `CATEGORY_DAILY_BONUS`).
    pub fn effective_goal_category(&self) -> u32 {
        if self.category == 6 {
            2
        } else {
            1
        }
    }

    /// Returns the effective EverMark reward (500 EM on regular `SDEDG` goals by default, 0 on DailyBonus).
    pub fn effective_evermarks_reward(&self) -> i64 {
        if self.category == 6 {
            0
        } else if self.evermarks_reward > 0 {
            self.evermarks_reward
        } else if self.uuid.starts_with(b"SDEDG") {
            // Legacy persisted SDE daily goals without `evermarks_reward` field retain 500 EM.
            500
        } else {
            0
        }
    }

    /// Returns the `res:/staticdata/contributionmethods.fsdbinary` `titleID` or configured message ID.
    pub fn effective_name_message_id(&self) -> u32 {
        if self.name_message_id > 0 {
            return self.name_message_id;
        }
        match self.category {
            1 => 659_195, // Destroy Non-Capsuleers
            2 => 659_218, // Mine Materials
            3 => 665_435, // Scan Signatures
            4 => 541_824, // Jump through Stargate
            5 => 665_394, // Manufacture
            6 => 722_674, // Daily Bonus
            7 => 659_201, // Damage Ship
            _ => 582_520, // Enforcer careerPath nameID
        }
    }

    /// Returns the `res:/staticdata/contributionmethods.fsdbinary` `progressDescriptionID` or configured ID.
    pub fn effective_description_message_id(&self) -> u32 {
        if self.description_message_id > 0 {
            return self.description_message_id;
        }
        match self.category {
            1 => 696_251, // Non-Capsuleers destroyed
            2 => 696_247, // Materials mined
            3 => 696_245, // Signatures scanned
            4 => 1_041_584, // Jump through a Stargate into a different Solar System
            5 => 696_246, // Items manufactured
            6 => 698_433, // Daily Bonus Rewards
            7 => 696_243, // Ship damage inflicted
            _ => 582_521, // Enforcer careerPath descriptionID
        }
    }

    /// Returns the `res:/staticdata/contributionmethods.fsdbinary` `descriptionID` or configured help text ID.
    pub fn effective_help_text_message_id(&self) -> u32 {
        if self.help_text_message_id > 0 {
            return self.help_text_message_id;
        }
        match self.category {
            1 => 659_196,   // Destroy ships piloted by non-capsuleers.
            2 => 659_219,   // Mine raw materials.
            3 => 665_436,   // Scan cosmic signatures to 100% resolution with a probe scanner.
            4 => 1_004_955, // You will earn progress towards this goal whenever you complete a jump between solar systems...
            5 => 665_412,   // Install manufacturing jobs for items of a specific type.
            6 => 698_433,   // Daily Bonus Rewards
            7 => 659_202,   // Inflict damage to another capsuleer's ship.
            _ => 582_521,
        }
    }

    /// Returns the assigning NPC corporation ID (defaults to `1_000_419`, Paragon).
    pub fn effective_assigner_corporation_id(&self) -> u32 {
        if self.assigner_corporation_id > 0 {
            self.assigner_corporation_id
        } else {
            1_000_419
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CharacterDailyGoal {
    pub uuid: [u8; 16],
    pub template: DailyGoalTemplate,
    pub current_progress: u64,
    pub completed: bool,
    pub claimed: bool,
    pub paid_completion: bool,
    pub completed_at_unix: Option<u64>,
    pub claimed_at_unix: Option<u64>,
    #[serde(default)]
    pub day_bucket: u64,
    #[serde(default)]
    pub expired: bool,
    #[serde(default)]
    pub prematurely_deactivated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MonthlyGoalProgress {
    /// `(year, month)` in UTC downtime calendar for the current 12-step Monthly Progress Track.
    pub year_month: (i32, u8),
    /// Completed bonus days in this UTC month (`0..=12`, max 1 step per downtime `day_bucket`).
    pub steps_completed: u32,
    /// Last `day_bucket` that incremented `steps_completed` (prevents duplicate increments same day).
    pub last_bonus_day_bucket: Option<u64>,
    /// Milestone thresholds (`3`, `6`, `9`, `12`) already claimed for `year_month`.
    #[serde(default)]
    pub claimed_milestones: Vec<u32>,
}

/// Canonical AIR Daily Goals Monthly Progress Track milestone steps (`AIR_Daily_Goals.md`).
pub const MONTHLY_TRACK_MAX_STEPS: u32 = 12;
pub const MONTHLY_TRACK_MILESTONES: [u32; 4] = [3, 6, 9, 12];

/// Fixed currency/SP reward grant for an AIR Daily Goals Monthly Progress Track milestone.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MonthlyMilestoneReward {
    pub alpha_isk: i64,
    pub alpha_em: i64,
    pub alpha_sp: f64,
    pub omega_isk: i64,
    pub omega_em: i64,
    pub omega_sp: f64,
}

/// Returns `(alpha_isk, alpha_em, alpha_sp, omega_isk, omega_em, omega_sp)` rewards for monthly milestones `3, 6, 9, 12`.
/// Total Alpha track: 200,000 ISK + 2,250 EverMarks + 25,000 SP; Omega adds 1,000,000 ISK + 9,000 EM + 125,000 SP.
pub fn monthly_milestone_rewards(milestone: u32) -> Option<MonthlyMilestoneReward> {
    match milestone {
        3 => Some(MonthlyMilestoneReward {
            alpha_isk: 200_000,
            omega_isk: 1_000_000,
            ..Default::default()
        }),
        6 => Some(MonthlyMilestoneReward {
            alpha_em: 2_250,
            omega_em: 9_000,
            ..Default::default()
        }),
        9 => Some(MonthlyMilestoneReward {
            alpha_sp: 25_000.0,
            omega_sp: 50_000.0,
            ..Default::default()
        }),
        12 => Some(MonthlyMilestoneReward {
            omega_sp: 75_000.0,
            ..Default::default()
        }),
        _ => None,
    }
}

/// Converts a UNIX timestamp to UTC calendar `(year, month)` (`1..=12`).
pub fn utc_year_month(unix_secs: u64) -> (i32, u8) {
    let z = (unix_secs / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year as i32, m as u8)
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DailyGoalStore {
    #[serde(default)]
    pub templates: Vec<DailyGoalTemplate>,
    #[serde(default)]
    pub character_goals: HashMap<u32, Vec<CharacterDailyGoal>>,
    #[serde(default)]
    pub monthly_progress: HashMap<u32, MonthlyGoalProgress>,
}

pub struct DailyGoalManager {
    pub templates: RwLock<Vec<DailyGoalTemplate>>,
    pub character_goals: RwLock<HashMap<u32, Vec<CharacterDailyGoal>>>,
    pub monthly_progress: RwLock<HashMap<u32, MonthlyGoalProgress>>,
    db: Option<Arc<WorldRepository>>,
}

impl Default for DailyGoalManager {
    fn default() -> Self {
        Self {
            templates: RwLock::new(Vec::new()),
            character_goals: RwLock::new(HashMap::new()),
            monthly_progress: RwLock::new(HashMap::new()),
            db: None,
        }
    }
}

/// Daily goals reset at 11:00 UTC downtime (39_600s offset from 00:00 UTC).
pub fn daily_goal_day_bucket(now_unix: u64) -> u64 {
    now_unix.saturating_sub(39_600) / 86_400
}

/// Returns `(active_after_unix, active_until_unix)` covering the 24h downtime bucket window.
pub fn daily_goal_window_unix(day_bucket: u64) -> (u64, u64) {
    let active_after = 39_600_u64.saturating_add(day_bucket.saturating_mul(86_400));
    let active_until = active_after.saturating_add(86_400);
    (active_after, active_until)
}

fn is_sde_rotating_regular_template(tpl: &DailyGoalTemplate) -> bool {
    tpl.uuid.starts_with(b"SDEDG") && tpl.category != 6
}

fn active_sde_regular_uuids(templates: &[DailyGoalTemplate], day_bucket: u64) -> Vec<[u8; 16]> {
    // DERIVED daily schedule (absent from V24 client static files): per AIR Daily Goals semantics,
    // select at most 1 regular SDE goal per career bucket (0 = General/Unspecified + 1..=4 Career Paths),
    // rotating deterministically across `day_bucket` when multiple SDE templates share a career slot
    // (e.g. Industrialist career 2 alternates MineOre vs ManufactureItem across days).
    let mut by_career: std::collections::BTreeMap<u32, Vec<[u8; 16]>> =
        std::collections::BTreeMap::new();
    for tpl in templates.iter().filter(|t| is_sde_rotating_regular_template(t)) {
        by_career.entry(tpl.career.min(4)).or_default().push(tpl.uuid);
    }
    by_career
        .into_values()
        .filter_map(|group| {
            let idx = (day_bucket as usize) % group.len();
            group.get(idx).copied()
        })
        .collect()
}

fn ensure_character_daily_goals_with_templates(
    goals: &mut Vec<CharacterDailyGoal>,
    templates: &[DailyGoalTemplate],
    now_unix: u64,
) -> bool {
    let mut changed = false;
    let day_bucket = if now_unix > 0 {
        daily_goal_day_bucket(now_unix)
    } else {
        goals.iter().map(|g| g.day_bucket).max().unwrap_or(0)
    };
    let active_sde_regular = active_sde_regular_uuids(templates, day_bucket);
    for tpl in templates {
        let is_active_today = if is_sde_rotating_regular_template(tpl) {
            active_sde_regular.contains(&tpl.uuid)
        } else {
            true
        };
        if let Some(existing) = goals.iter_mut().find(|g| g.uuid == tpl.uuid) {
            existing.template = tpl.clone();
            if existing.day_bucket > 0 && day_bucket > existing.day_bucket {
                existing.current_progress = 0;
                existing.completed = false;
                existing.claimed = false;
                existing.paid_completion = false;
                existing.completed_at_unix = None;
                existing.claimed_at_unix = None;
                existing.expired = !is_active_today;
                existing.prematurely_deactivated = false;
                existing.day_bucket = day_bucket;
                changed = true;
            } else if existing.day_bucket == 0 && day_bucket > 0 {
                existing.day_bucket = day_bucket;
                if existing.prematurely_deactivated || !is_active_today {
                    existing.expired = true;
                } else if existing.current_progress == 0
                    && !existing.completed
                    && !existing.claimed
                    && !existing.paid_completion
                {
                    existing.expired = false;
                }
                changed = true;
            } else if !is_active_today && !existing.expired {
                existing.expired = true;
                changed = true;
            }
        } else {
            goals.push(CharacterDailyGoal {
                uuid: tpl.uuid,
                template: tpl.clone(),
                current_progress: 0,
                completed: false,
                claimed: false,
                paid_completion: false,
                completed_at_unix: None,
                claimed_at_unix: None,
                day_bucket,
                expired: !is_active_today,
                prematurely_deactivated: false,
            });
            changed = true;
        }
    }
    changed
}

pub fn default_daily_bonus_template() -> DailyGoalTemplate {
    // DERIVED deterministic server UUID (`b"SDEDG..."`): V24 client files contain localization/method
    // definitions but not server-issued daily goal instance UUIDs.
    let mut uuid = [0u8; 16];
    uuid[0..5].copy_from_slice(b"SDEDG");
    uuid[15] = 6;
    DailyGoalTemplate {
        uuid,
        name_message: "DailyBonus".to_string(),
        description_message: "DailyBonus_daily".to_string(),
        category: 6,
        career: 0,
        target: 2,
        isk_reward: 0,
        evermarks_reward: 0,
        sp_reward: 10_000.0,
        completion_cost_isk: None,
        completion_cost_evermarks: None,
        name_message_id: 722_674,
        description_message_id: 698_433,
        help_text_message_id: 698_433,
        assigner_corporation_id: 1_000_419,
    }
}

fn daily_goal_catalog_message_id(
    catalog: &HashMap<&str, u32>,
    label: &str,
) -> Option<u32> {
    catalog.get(label).copied().filter(|&id| id > 0)
}

/// Builds the daily bonus template using client localization IDs from `sde.daily_goal_catalog`.
pub fn default_daily_bonus_template_for_sde(sde: &eve_sde::SdeRegistry) -> DailyGoalTemplate {
    let catalog: HashMap<&str, u32> = sde
        .daily_goal_catalog
        .iter()
        .map(|entry| (entry.label.as_str(), entry.message_id))
        .collect();
    let mut tpl = default_daily_bonus_template();
    if let Some(id) = daily_goal_catalog_message_id(&catalog, "DailyCompletionBonus_name") {
        tpl.name_message_id = id;
    }
    if let Some(id) = daily_goal_catalog_message_id(&catalog, "DailyCompletionBonus_description") {
        tpl.description_message_id = id;
    }
    if let Some(id) = daily_goal_catalog_message_id(&catalog, "DailyCompletionBonus_help") {
        tpl.help_text_message_id = id;
    }
    tpl
}

/// Builds daily goal templates combining V24 client tables (`sde_daily_goal_catalog` and
/// `sde_contribution_methods` in `evomu_sde.sqlite`) with **DERIVED** server policy defaults
/// (`b"SDEDG..."` deterministic UUIDs, targets, 500,000 ISK + 500 EM payouts per `AIR_Daily_Goals.md`,
/// `PRICE_FIRST_COMPLETION = 2,500` EM, and per-career daily slot rotation; targets, payouts, UUIDs,
/// and daily schedule do not exist in V24 client static files).
pub fn build_sde_daily_goal_templates(sde: &eve_sde::SdeRegistry) -> Vec<DailyGoalTemplate> {
    let catalog: HashMap<&str, u32> = sde
        .daily_goal_catalog
        .iter()
        .map(|entry| (entry.label.as_str(), entry.message_id))
        .collect();
    let cfsd_methods = sde.contribution_methods().unwrap_or_default();
    struct DailyGoalSpec {
        method_key: &'static str,
        name_label: &'static str,
        desc_label: &'static str,
        help_label: &'static str,
        category: u32,
        career: u32,
        target: u64,
        isk_reward: i64,
        evermarks_reward: i64,
        sp_reward: f64,
        completion_cost_isk: Option<i64>,
        completion_cost_evermarks: Option<i64>,
    }
    // Localization IDs come from V24 `UI/DailyGoals` & `contributionmethods.fsdbinary`;
    // targets (`25, 2_000, 5, 3, 1, 2_000`), payouts (`500_000 ISK + 500 EM`), and daily rotation are DERIVED.
    let specs: [DailyGoalSpec; 6] = [
        DailyGoalSpec {
            method_key: "KillNPC",
            name_label: "DestroyNoncapsuleers_name",
            desc_label: "DestroyNon-capsuleers_description",
            help_label: "DestroyNoncapsuleers_help",
            category: 1,
            career: 3, // Enforcer
            target: 25,
            isk_reward: 500_000,
            evermarks_reward: 500,
            sp_reward: 0.0,
            completion_cost_isk: None,
            completion_cost_evermarks: Some(2_500),
        },
        DailyGoalSpec {
            method_key: "MineOre",
            name_label: "MineOre_name",
            desc_label: "Mine2000Ore_description",
            help_label: "MineOre_help",
            category: 2,
            career: 2, // Industrialist (alternates daily with ManufactureItem)
            target: 2_000,
            isk_reward: 500_000,
            evermarks_reward: 500,
            sp_reward: 0.0,
            completion_cost_isk: None,
            completion_cost_evermarks: Some(2_500),
        },
        DailyGoalSpec {
            method_key: "ScanSignature",
            name_label: "ScanSignatures_name",
            // Omit `Scan15_description` (`Scan 15 signatures`) since target = 5; fall back cleanly to `Signatures scanned`.
            desc_label: "",
            help_label: "ScanSignatures_help",
            category: 3,
            career: 1, // Explorer
            target: 5,
            isk_reward: 500_000,
            evermarks_reward: 500,
            sp_reward: 0.0,
            completion_cost_isk: None,
            completion_cost_evermarks: Some(2_500),
        },
        DailyGoalSpec {
            method_key: "SpaceJump",
            name_label: "JumpGoal_name",
            desc_label: "JumpGoal_description",
            help_label: "JumpGoal_help",
            category: 4,
            career: 0, // General (`CAREER_UNSPECIFIED`)
            target: 3,
            isk_reward: 500_000,
            evermarks_reward: 500,
            sp_reward: 0.0,
            completion_cost_isk: None,
            completion_cost_evermarks: Some(2_500),
        },
        DailyGoalSpec {
            method_key: "ManufactureItem",
            name_label: "ManufactureItem_name",
            desc_label: "ManufactureItem_description",
            help_label: "ManufactureItem_help",
            category: 5,
            career: 2, // Industrialist (alternates daily with MineOre)
            target: 1,
            isk_reward: 500_000,
            evermarks_reward: 500,
            sp_reward: 0.0,
            completion_cost_isk: None,
            completion_cost_evermarks: Some(2_500),
        },
        DailyGoalSpec {
            method_key: "DamageShip",
            name_label: "DamageCapsuleers_name",
            desc_label: "DamageCapsuleers_description",
            help_label: "DamageCapsuleers_help",
            category: 7,
            career: 4, // Soldier of Fortune
            // DERIVED server threshold (absent from V24 client FSD).
            target: 2_000,
            isk_reward: 500_000,
            evermarks_reward: 500,
            sp_reward: 0.0,
            completion_cost_isk: None,
            completion_cost_evermarks: Some(2_500),
        },
    ];
    specs
        .into_iter()
        .map(|spec| {
            let sqlite_m = sde.contribution_methods.get(spec.method_key);
            let cfsd_m = cfsd_methods.get(spec.method_key);
            let name_id = daily_goal_catalog_message_id(&catalog, spec.name_label)
                .or_else(|| sqlite_m.map(|m| m.title_id).filter(|&id| id > 0))
                .or_else(|| {
                    cfsd_m
                        .and_then(|v| v.get("titleID"))
                        .and_then(|v| v.as_u64())
                        .map(|v| v as u32)
                        .filter(|&id| id > 0)
                })
                .unwrap_or(0);
            let desc_id = daily_goal_catalog_message_id(&catalog, spec.desc_label)
                .or_else(|| {
                    sqlite_m
                        .map(|m| m.progress_description_id)
                        .filter(|&id| id > 0)
                })
                .or_else(|| {
                    cfsd_m
                        .and_then(|v| v.get("progressDescriptionID"))
                        .and_then(|v| v.as_u64())
                        .map(|v| v as u32)
                        .filter(|&id| id > 0)
                })
                .unwrap_or(0);
            let help_id = daily_goal_catalog_message_id(&catalog, spec.help_label)
                .or_else(|| sqlite_m.map(|m| m.description_id).filter(|&id| id > 0))
                .or_else(|| {
                    cfsd_m
                        .and_then(|v| v.get("descriptionID"))
                        .and_then(|v| v.as_u64())
                        .map(|v| v as u32)
                        .filter(|&id| id > 0)
                })
                .unwrap_or(0);
            // DERIVED synthetic deterministic UUID (`b"SDEDG..."`): not sourced from V24 client files.
            let mut uuid = [0u8; 16];
            uuid[0..5].copy_from_slice(b"SDEDG");
            uuid[15] = spec.category as u8;
            DailyGoalTemplate {
                uuid,
                name_message: spec.method_key.to_string(),
                description_message: format!("{}_daily", spec.method_key),
                category: spec.category,
                career: spec.career,
                target: spec.target,
                isk_reward: spec.isk_reward,
                evermarks_reward: spec.evermarks_reward,
                sp_reward: spec.sp_reward,
                completion_cost_isk: spec.completion_cost_isk,
                completion_cost_evermarks: spec.completion_cost_evermarks,
                name_message_id: name_id,
                description_message_id: desc_id,
                help_text_message_id: help_id,
                assigner_corporation_id: 1_000_419,
            }
        })
        .chain(std::iter::once(default_daily_bonus_template_for_sde(sde)))
        .collect()
}

impl DailyGoalManager {
    pub fn new(db: Option<Arc<WorldRepository>>) -> Self {
        Self::new_with_sde(db, None)
    }

    pub fn new_with_sde(
        db: Option<Arc<WorldRepository>>,
        sde: Option<Arc<eve_sde::SdeRegistry>>,
    ) -> Self {
        let mut store = db
            .as_ref()
            .map(|d| {
                d.with_snapshot(|s| {
                    s.quasar_state_blobs
                        .get("quasar_daily_goals")
                        .and_then(|json| {
                            if let Some(st) = serde_json::from_str::<DailyGoalStore>(json)
                                .ok()
                                .filter(|st| !st.templates.is_empty() || !st.character_goals.is_empty())
                            {
                                Some(st)
                            } else if let Ok(cg) =
                                serde_json::from_str::<HashMap<u32, Vec<CharacterDailyGoal>>>(json)
                            {
                                let mut templates = Vec::new();
                                for list in cg.values() {
                                    for g in list {
                                        if !templates.iter().any(|t: &DailyGoalTemplate| t.uuid == g.template.uuid) {
                                            templates.push(g.template.clone());
                                        }
                                    }
                                }
                                Some(DailyGoalStore {
                                    templates,
                                    character_goals: cg,
                                    monthly_progress: HashMap::new(),
                                })
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default();

        if store.templates.is_empty() {
            if let Some(ref sde_reg) = sde {
                store.templates = build_sde_daily_goal_templates(sde_reg);
            }
        }
        if !store.templates.is_empty()
            && !store.templates.iter().any(|t| t.effective_goal_category() == 2)
        {
            let bonus = sde
                .as_ref()
                .map(|r| default_daily_bonus_template_for_sde(r))
                .unwrap_or_else(default_daily_bonus_template);
            store.templates.push(bonus);
        }

        let mut character_goals = store.character_goals;
        for goals in character_goals.values_mut() {
            ensure_character_daily_goals_with_templates(goals, &store.templates, 0);
        }

        Self {
            templates: RwLock::new(store.templates),
            character_goals: RwLock::new(character_goals),
            monthly_progress: RwLock::new(store.monthly_progress),
            db,
        }
    }

    pub fn get_monthly_progress(&self, character_id: u32, now_unix: u64) -> MonthlyGoalProgress {
        let ym = utc_year_month(if now_unix > 0 { now_unix } else { self.now_unix() });
        let map = self.monthly_progress.read().unwrap_or_else(|e| e.into_inner());
        match map.get(&character_id) {
            Some(p) if p.year_month == ym => p.clone(),
            _ => MonthlyGoalProgress {
                year_month: ym,
                steps_completed: 0,
                last_bonus_day_bucket: None,
                claimed_milestones: Vec::new(),
            },
        }
    }

    pub fn record_daily_bonus_completion(&self, character_id: u32, now_unix: u64) -> MonthlyGoalProgress {
        let effective_now = if now_unix > 0 { now_unix } else { self.now_unix() };
        let ym = utc_year_month(effective_now);
        let day_bucket = daily_goal_day_bucket(effective_now);
        let mut map = self.monthly_progress.write().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(character_id).or_insert_with(|| MonthlyGoalProgress {
            year_month: ym,
            steps_completed: 0,
            last_bonus_day_bucket: None,
            claimed_milestones: Vec::new(),
        });
        if entry.year_month != ym {
            *entry = MonthlyGoalProgress {
                year_month: ym,
                steps_completed: 0,
                last_bonus_day_bucket: None,
                claimed_milestones: Vec::new(),
            };
        }
        if entry.last_bonus_day_bucket != Some(day_bucket)
            && entry.steps_completed < MONTHLY_TRACK_MAX_STEPS
        {
            entry.last_bonus_day_bucket = Some(day_bucket);
            entry.steps_completed += 1;
        }
        entry.clone()
    }

    pub fn claim_monthly_milestone(
        &self,
        character_id: u32,
        milestone: u32,
        is_omega: bool,
        now_unix: u64,
        db: &WorldRepository,
    ) -> Result<MonthlyGoalProgress, (u32, String)> {
        let Some(MonthlyMilestoneReward {
            alpha_isk: a_isk,
            alpha_em: a_em,
            alpha_sp: a_sp,
            omega_isk: o_isk,
            omega_em: o_em,
            omega_sp: o_sp,
        }) = monthly_milestone_rewards(milestone) else {
            return Err((STATUS_BAD_REQUEST, format!("Invalid monthly milestone step: {milestone}")));
        };
        let effective_now = if now_unix > 0 { now_unix } else { self.now_unix() };
        let ym = utc_year_month(effective_now);
        {
            let mut map = self.monthly_progress.write().unwrap_or_else(|e| e.into_inner());
            let entry = map.entry(character_id).or_insert_with(|| MonthlyGoalProgress {
                year_month: ym,
                steps_completed: 0,
                last_bonus_day_bucket: None,
                claimed_milestones: Vec::new(),
            });
            if entry.year_month != ym {
                *entry = MonthlyGoalProgress {
                    year_month: ym,
                    steps_completed: 0,
                    last_bonus_day_bucket: None,
                    claimed_milestones: Vec::new(),
                };
            }
            if entry.steps_completed < milestone {
                return Err((STATUS_PRECONDITION_FAILED, "Monthly milestone not yet reached".into()));
            }
            if entry.claimed_milestones.contains(&milestone) {
                return Err((STATUS_FORBIDDEN, "Monthly milestone already claimed".into()));
            }
            entry.claimed_milestones.push(milestone);
        }

        let isk_total = a_isk + if is_omega { o_isk } else { 0 };
        let em_total = a_em + if is_omega { o_em } else { 0 };
        let sp_total = a_sp + if is_omega { o_sp } else { 0.0 };
        let credit_res = db.try_with_mut_snapshot(|snap| {
            crate::domain::career_goals::credit_character_reward_in_snap(
                snap,
                character_id as i64,
                IskCents::from_isk(isk_total),
                sp_total,
                em_total,
                Some((
                    "DailyGoalReward",
                    format!("AIR Monthly Progress Track milestone {milestone}"),
                    None,
                )),
            )
            .map(|_| ())
            .map_err(|e| e.message)
        });
        if credit_res.is_err() {
            let mut map = self.monthly_progress.write().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = map.get_mut(&character_id) {
                entry.claimed_milestones.retain(|&m| m != milestone);
            }
            return Err((STATUS_BAD_REQUEST, "Unable to credit monthly milestone reward".into()));
        }
        let updated = self.get_monthly_progress(character_id, effective_now);
        self.save_to_db();
        Ok(updated)
    }

    pub fn reset_daily_goals_on_downtime(&self, now_unix: u64) -> usize {
        let mut count = 0;
        let tpls = self.templates.read().unwrap().clone();
        let mut chars = self.character_goals.write().unwrap();
        for goals in chars.values_mut() {
            if ensure_character_daily_goals_with_templates(goals, &tpls, now_unix) {
                count += 1;
            }
        }
        if count > 0 {
            drop(chars);
            self.save_to_db();
        }
        count
    }

    pub fn save_to_db(&self) {
        if let Some(ref db) = self.db {
            if db.is_write_locked_by_current_thread() {
                return;
            }
            let templates = self.templates.read().unwrap_or_else(|e| e.into_inner()).clone();
            let character_goals = self.character_goals.read().unwrap_or_else(|e| e.into_inner()).clone();
            let monthly_progress = self.monthly_progress.read().unwrap_or_else(|e| e.into_inner()).clone();
            let store = DailyGoalStore { templates, character_goals, monthly_progress };
            if let Ok(json) = serde_json::to_string(&store) {
                let already = db.with_snapshot(|snap| {
                    snap.quasar_state_blobs
                        .get("quasar_daily_goals")
                        .is_some_and(|cur| cur == &json)
                });
                if !already {
                    let _ = db.try_with_mut_snapshot(|snap| {
                        snap.quasar_state_blobs.insert("quasar_daily_goals".into(), json);
                        Ok::<(), String>(())
                    });
                }
            }
        }
    }

    pub fn rehydrate_from_db(&self) {
        let Some(ref db) = self.db else { return };
        let store_opt = db.with_snapshot(|s| {
            s.quasar_state_blobs.get("quasar_daily_goals").and_then(|json| {
                if let Some(st) = serde_json::from_str::<DailyGoalStore>(json)
                    .ok()
                    .filter(|st| !st.templates.is_empty() || !st.character_goals.is_empty())
                {
                    Some(st)
                } else if let Ok(cg) =
                    serde_json::from_str::<HashMap<u32, Vec<CharacterDailyGoal>>>(json)
                {
                    let mut templates = Vec::new();
                    for list in cg.values() {
                        for g in list {
                            if !templates.iter().any(|t: &DailyGoalTemplate| t.uuid == g.template.uuid) {
                                templates.push(g.template.clone());
                            }
                        }
                    }
                    Some(DailyGoalStore {
                        templates,
                        character_goals: cg,
                        monthly_progress: HashMap::new(),
                    })
                } else {
                    None
                }
            })
        });
        if let Some(mut store) = store_opt {
            if !store.templates.is_empty()
                && !store.templates.iter().any(|t| t.effective_goal_category() == 2)
            {
                store.templates.push(default_daily_bonus_template());
            }
            if !store.templates.is_empty() {
                *self.templates.write().unwrap_or_else(|e| e.into_inner()) = store.templates.clone();
            }
            let tpls = self.templates.read().unwrap_or_else(|e| e.into_inner()).clone();
            for goals in store.character_goals.values_mut() {
                ensure_character_daily_goals_with_templates(goals, &tpls, 0);
            }
            *self
                .character_goals
                .write()
                .unwrap_or_else(|e| e.into_inner()) = store.character_goals;
            *self
                .monthly_progress
                .write()
                .unwrap_or_else(|e| e.into_inner()) = store.monthly_progress;
        } else {
            self.character_goals
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            self.monthly_progress
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
    }

    pub fn configure_template(&self, tpl: DailyGoalTemplate) {
        let templates = {
            let mut tpls = self.templates.write().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = tpls.iter_mut().find(|t| t.uuid == tpl.uuid) {
                *existing = tpl.clone();
            } else {
                tpls.push(tpl.clone());
            }
            tpls.clone()
        };
        {
            let mut map = self.character_goals.write().unwrap_or_else(|e| e.into_inner());
            for goals in map.values_mut() {
                ensure_character_daily_goals_with_templates(goals, &templates, 0);
            }
        }
        self.save_to_db();
    }

    pub fn set_templates(&self, new_templates: Vec<DailyGoalTemplate>) {
        {
            let mut tpls = self.templates.write().unwrap_or_else(|e| e.into_inner());
            *tpls = new_templates.clone();
        }
        {
            let mut map = self.character_goals.write().unwrap_or_else(|e| e.into_inner());
            for goals in map.values_mut() {
                ensure_character_daily_goals_with_templates(goals, &new_templates, 0);
            }
        }
        self.save_to_db();
    }

    pub fn get_templates(&self) -> Vec<DailyGoalTemplate> {
        self.templates.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn now_unix(&self) -> u64 {
        self.db
            .as_ref()
            .map(|db| db.clock().now_unix_secs())
            .unwrap_or(0)
    }

    fn ensure_character_goals(&self, goals: &mut Vec<CharacterDailyGoal>, now_unix: u64) -> bool {
        let tpls = self.templates.read().unwrap_or_else(|e| e.into_inner());
        ensure_character_daily_goals_with_templates(goals, &tpls, now_unix)
    }

    pub fn get_or_init_goals(&self, character_id: u32) -> Vec<CharacterDailyGoal> {
        self.get_or_init_goals_at(character_id, self.now_unix())
    }

    pub fn get_or_init_goals_at(&self, character_id: u32, now_unix: u64) -> Vec<CharacterDailyGoal> {
        let effective_now = if now_unix > 0 { now_unix } else { self.now_unix() };
        let (out, changed) = {
            let mut map = self
                .character_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let is_new_char = !map.contains_key(&character_id);
            let goals = map.entry(character_id).or_default();
            let mutated = self.ensure_character_goals(goals, effective_now);
            let active = goals.iter().filter(|g| !g.expired).cloned().collect();
            (active, is_new_char || mutated)
        };
        if changed {
            self.save_to_db();
        }
        out
    }

    /// Resolves a daily goal for single-UUID `GetRequest` lookup across active goals,
    /// persisted historical/expired character goals (excluding prematurely deactivated ones),
    /// and known catalog templates.
    pub fn resolve_goal_for_get_at(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        now_unix: u64,
    ) -> Option<ResolvedDailyGoal> {
        let effective_now = if now_unix > 0 { now_unix } else { self.now_unix() };
        let active_goals = self.get_or_init_goals_at(character_id, effective_now);
        if let Some(active) = active_goals.into_iter().find(|g| &g.uuid == goal_uuid) {
            return Some(ResolvedDailyGoal {
                template: active.template,
                current_progress: active.current_progress,
                day_bucket: active.day_bucket,
                is_expired_or_catalog: false,
            });
        }
        self.find_expired_or_template_goal(character_id, goal_uuid, effective_now)
    }

    /// Fallback lookup for `GetRequest` when `goal_uuid` is absent from active goals:
    /// checks persisted `character_goals` (honoring `prematurely_deactivated` -> `None`),
    /// then falls back to known `templates` without resurrecting deactivated goals.
    fn find_expired_or_template_goal(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        effective_now: u64,
    ) -> Option<ResolvedDailyGoal> {
        let persisted = {
            let map = self
                .character_goals
                .read()
                .unwrap_or_else(|e| e.into_inner());
            map.get(&character_id)
                .and_then(|list| list.iter().find(|g| &g.uuid == goal_uuid).cloned())
        };
        if let Some(g) = persisted {
            if g.prematurely_deactivated {
                return None;
            }
            return Some(ResolvedDailyGoal {
                template: g.template,
                current_progress: g.current_progress,
                day_bucket: g.day_bucket,
                is_expired_or_catalog: true,
            });
        }

        let catalog_day_bucket = if effective_now > 0 {
            daily_goal_day_bucket(effective_now)
        } else {
            0
        };
        self.get_templates()
            .into_iter()
            .find(|t| &t.uuid == goal_uuid)
            .map(|template| ResolvedDailyGoal {
                template,
                current_progress: 0,
                day_bucket: catalog_day_bucket,
                is_expired_or_catalog: true,
            })
    }

    pub fn add_progress(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        progress_delta: u64,
        now_unix: u64,
    ) -> Option<(CharacterDailyGoal, bool)> {
        let mut map = self
            .character_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let goals = map.entry(character_id).or_default();
        self.ensure_character_goals(goals, now_unix);

        let mut res = None;
        for g in goals.iter_mut() {
            if &g.uuid == goal_uuid && !g.expired {
                let prev_completed = g.completed;
                g.current_progress = g.current_progress.saturating_add(progress_delta);
                if g.current_progress >= g.template.target && !g.completed {
                    g.completed = true;
                    g.completed_at_unix = Some(now_unix);
                }
                let just_completed = !prev_completed && g.completed;
                res = Some((g.clone(), just_completed));
                break;
            }
        }
        let mut bonus_just_completed = false;
        if res.is_some() {
            let num_completed_regular = goals
                .iter()
                .filter(|goal| goal.template.category != 6 && goal.completed && !goal.expired)
                .count() as u64;
            for bonus in goals.iter_mut() {
                if bonus.template.category == 6 && !bonus.completed && !bonus.expired {
                    let prev_b = bonus.completed;
                    bonus.current_progress = num_completed_regular;
                    if bonus.current_progress >= bonus.template.target {
                        bonus.completed = true;
                        bonus.completed_at_unix = Some(now_unix);
                    }
                    if !prev_b && bonus.completed {
                        bonus_just_completed = true;
                    }
                }
            }
        }
        drop(map);
        if bonus_just_completed {
            self.record_daily_bonus_completion(character_id, now_unix);
        }
        res
    }

    pub fn redeem_goal(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        now_unix: u64,
        db: &WorldRepository,
    ) -> Result<CharacterDailyGoal, (u32, String)> {
        let (res_goal, isk, sp) = {
            let mut map = self
                .character_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let Some(goals) = map.get_mut(&character_id) else {
                return Err((STATUS_NOT_FOUND, "No daily goals found for character".into()));
            };
            self.ensure_character_goals(goals, now_unix);

            let mut found = None;
            for g in goals.iter_mut() {
                if &g.uuid == goal_uuid && !g.expired {
                    if !g.completed {
                        return Err((STATUS_FORBIDDEN, "Daily goal not yet completed".into()));
                    }
                    if g.claimed {
                        return Err((STATUS_FORBIDDEN, "Reward already claimed".into()));
                    }
                    g.claimed = true;
                    g.claimed_at_unix = Some(now_unix);
                    found = Some((g.clone(), g.template.isk_reward, g.template.sp_reward));
                    break;
                }
            }
            match found {
                Some(f) => f,
                None => return Err((STATUS_NOT_FOUND, "Daily goal not found".into())),
            }
        };

        let reward_cents = IskCents::from_isk(isk);
        let evermarks_reward = res_goal.template.effective_evermarks_reward();
        let credit_res = db.try_with_mut_snapshot(|snap| {
            crate::domain::career_goals::credit_character_reward_in_snap(
                snap,
                character_id as i64,
                reward_cents,
                sp,
                evermarks_reward,
                Some((
                    "DailyGoalReward",
                    format!("Daily goal reward ({})", res_goal.template.name_message),
                    None,
                )),
            )
            .map(|_| ())
            .map_err(|e| e.message)
        });
        if credit_res.is_err() {
            let mut map = self
                .character_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(goals) = map.get_mut(&character_id) {
                for g in goals.iter_mut() {
                    if &g.uuid == goal_uuid {
                        g.claimed = false;
                        g.claimed_at_unix = None;
                        break;
                    }
                }
            }
            return Err((STATUS_BAD_REQUEST, "Unable to credit daily goal reward".into()));
        }

        Ok(res_goal)
    }

    pub fn pay_for_completion(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        now_unix: u64,
        db: &Arc<WorldRepository>,
    ) -> Result<CharacterDailyGoal, (u32, String)> {
        // 1. Validate goal existence and progressive EverMarks completion cost (2,500 -> 4,500 EM) under short-lived lock
        let (em_cost, assigner_corp) = {
            let mut map = self
                .character_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let goals = map.entry(character_id).or_default();
            self.ensure_character_goals(goals, now_unix);
            let Some(g) = goals.iter().find(|g| &g.uuid == goal_uuid && !g.expired) else {
                return Err((STATUS_NOT_FOUND, "Daily goal not found".into()));
            };
            if g.template.category == 6 {
                return Err((
                    STATUS_PRECONDITION_FAILED,
                    "Daily bonus goal cannot be completed with paid completion".into(),
                ));
            }
            if g.completed {
                return Err((STATUS_BAD_REQUEST, "Daily goal already completed".into()));
            }
            let paid_count = goals
                .iter()
                .filter(|goal| goal.template.category != 6 && goal.paid_completion && !goal.expired)
                .count();
            let bonus_prog = goals
                .iter()
                .find(|goal| goal.template.category == 6 && !goal.expired)
                .map(|b| if b.completed { 2 } else { b.current_progress as usize })
                .unwrap_or(0);
            if 2usize.saturating_sub(bonus_prog.max(paid_count)) == 0 {
                return Err((
                    STATUS_PRECONDITION_FAILED,
                    "Daily Skill Point bonus already reached; paid completion unavailable".into(),
                ));
            }
            if !g.template.completion_cost_evermarks.is_some_and(|c| c > 0) {
                return Err((
                    STATUS_PRECONDITION_FAILED,
                    "Paid completion cost is not configured for this daily goal".into(),
                ));
            }
            let cost_em: i64 = if paid_count == 0 { 2_500 } else { 4_500 };
            (cost_em, g.template.effective_assigner_corporation_id() as i64)
        };

        // 2. Debit EverMarks via try_with_mut_snapshot without holding character_goals lock
        let debit_res = db.try_with_mut_snapshot(|snap| {
            let Some(ch) = snap.characters.get_mut(&(character_id as i64)) else {
                return Err("Character not found".to_string());
            };
            let em_bal = ch.loyalty_points.get(&assigner_corp).copied().unwrap_or(0);
            if em_bal >= em_cost {
                *ch.loyalty_points.entry(assigner_corp).or_insert(0) -= em_cost;
                return Ok(assigner_corp);
            }
            if assigner_corp != 1_000_419 {
                let paragon_bal = ch.loyalty_points.get(&1_000_419).copied().unwrap_or(0);
                if paragon_bal >= em_cost {
                    *ch.loyalty_points.entry(1_000_419).or_insert(0) -= em_cost;
                    return Ok(1_000_419);
                }
            }
            Err("Insufficient EverMarks to pay for daily goal completion".to_string())
        });
        let debited_corp = match debit_res {
            Ok(corp) => corp,
            Err(msg) => return Err((STATUS_BAD_REQUEST, msg)),
        };

        // 3. Mark goal complete under write lock without holding across db.try_with_mut_snapshot
        let (outcome, bonus_just_completed) = {
            let mut map = self
                .character_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let mut found = None;
            let mut bonus_completed_now = false;
            if let Some(goals) = map.get_mut(&character_id) {
                for g in goals.iter_mut() {
                    if &g.uuid == goal_uuid {
                        if g.completed {
                            found = Some(Err((STATUS_BAD_REQUEST, "Daily goal already completed".into())));
                        } else {
                            g.current_progress = g.template.target;
                            g.completed = true;
                            g.paid_completion = true;
                            g.completed_at_unix = Some(now_unix);
                            found = Some(Ok(g.clone()));
                        }
                        let num_completed_regular = goals
                            .iter()
                            .filter(|goal| goal.template.category != 6 && goal.completed && !goal.expired)
                            .count() as u64;
                        for bonus in goals.iter_mut() {
                            if bonus.template.category == 6 && !bonus.completed && !bonus.expired {
                                let prev_b = bonus.completed;
                                bonus.current_progress = num_completed_regular;
                                if bonus.current_progress >= bonus.template.target {
                                    bonus.completed = true;
                                    bonus.completed_at_unix = Some(now_unix);
                                }
                                if !prev_b && bonus.completed {
                                    bonus_completed_now = true;
                                }
                            }
                        }
                        break;
                    }
                }
            }
            (found, bonus_completed_now)
        };

        let rollback_payment = |db: &Arc<WorldRepository>| {
            let _ = db.try_with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&(character_id as i64)) {
                    let cur = ch.loyalty_points.entry(debited_corp).or_insert(0);
                    *cur = cur.saturating_add(em_cost);
                }
                Ok::<(), String>(())
            });
        };

        match outcome {
            Some(Ok(g)) => {
                if bonus_just_completed {
                    self.record_daily_bonus_completion(character_id, now_unix);
                }
                self.save_to_db();
                Ok(g)
            }
            Some(Err(e)) => {
                rollback_payment(db);
                Err(e)
            }
            None => {
                rollback_payment(db);
                Err((STATUS_NOT_FOUND, "Daily goal not found".into()))
            }
        }
    }

    pub fn on_gameplay_event(
        &self,
        char_id: u32,
        category: u32,
        delta: u64,
        gateway: &PublicGatewayRouter,
    ) {
        crate::quasar_gateway::isolate_side_effect("quasar_daily_goals.on_gameplay_event", || {
            let now_unix = self.now_unix();
            let mut progressed = Vec::new();
            let mut bonus_just_completed = false;
            {
                let mut map = self
                    .character_goals
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                let goals = map.entry(char_id).or_default();
                self.ensure_character_goals(goals, now_unix);

                for g in goals.iter_mut() {
                    if g.template.category == category && !g.completed && !g.expired {
                        let prev = g.completed;
                        g.current_progress = g.current_progress.saturating_add(delta);
                        if g.current_progress >= g.template.target {
                            g.completed = true;
                            g.completed_at_unix = Some(now_unix);
                        }
                        let just_completed = !prev && g.completed;
                        progressed.push((g.uuid, g.current_progress, just_completed));
                    }
                }

                let num_completed_regular = goals
                    .iter()
                    .filter(|g| g.template.category != 6 && g.completed && !g.expired)
                    .count() as u64;
                for g in goals.iter_mut() {
                    if g.template.category == 6 && !g.completed && !g.expired {
                        let prev = g.completed;
                        g.current_progress = num_completed_regular;
                        if g.current_progress >= g.template.target {
                            g.completed = true;
                            g.completed_at_unix = Some(now_unix);
                        }
                        let just_completed = !prev && g.completed;
                        if just_completed {
                            bonus_just_completed = true;
                        }
                        if g.current_progress > 0 {
                            progressed.push((g.uuid, g.current_progress, just_completed));
                        }
                    }
                }
            }
            if bonus_just_completed {
                self.record_daily_bonus_completion(char_id, now_unix);
            }
            self.save_to_db();

            for (uuid, current_progress, just_completed) in progressed {
                let _ = gateway.publish_pb_notice(
                    NoticeScope::Character(char_id),
                    "quasar_daily_goals.on_gameplay_event.progressed",
                    pb::message("eve_public.dailygoal.api.ProgressedNotice")
                        .set("goal.uuid", uuid)
                        .set("current_progress", current_progress),
                    now_unix,
                );

                if just_completed {
                    let _ = gateway.publish_pb_notice(
                        NoticeScope::Character(char_id),
                        "quasar_daily_goals.on_gameplay_event.completed",
                        pb::message("eve_public.dailygoal.api.CompletedNotice")
                            .set("goal.uuid", uuid),
                        now_unix,
                    );
                }
            }
        });
    }
}

// ============================================================================
// Protobuf Wire Encoders for Daily Goals
// ============================================================================

/// Encodes `eve_public.dailygoal.Identifier` / `eve.dailygoal.Identifier`: `1: bytes uuid`.
pub fn encode_goal_identifier(uuid: &[u8; 16], out: &mut Vec<u8>) {
    crate::quasar_gateway::pb_encode_into_or_log(
        "quasar_daily_goals.encode_goal_identifier",
        pb::message("eve_public.dailygoal.Identifier").set("uuid", uuid),
        out,
    );
}

#[cfg_attr(not(test), allow(dead_code))]
fn encode_loc_message_id(seq: u32) -> Vec<u8> {
    crate::quasar_gateway::pb_encode_or_log(
        "quasar_daily_goals.encode_loc_message_id",
        pb::message("eve_public.localization.message.Identifier").set("sequential", seq),
    )
    .unwrap_or_default()
}

fn fill_contribution_configuration(cfg: ProtoBuilder, category: u32) -> ProtoBuilder {
    match category {
        1 => cfg.set_with("kill_npc", |k| {
            k.add_with("contribution_methods", |m| m.set("solar_system.any", true))
        }),
        2 => cfg.set_with("mine_ore", |k| {
            k.add_with("contribution_methods", |m| {
                m.set("solar_system.any", true).set("ore.any", true)
            })
        }),
        3 => cfg.set_with("scan_signature", |k| {
            k.add_with("contribution_methods", |m| {
                m.set("signature_type.any", true)
                    .set("solar_system.any", true)
            })
        }),
        4 => cfg.set_with("space_jump", |m| m),
        5 => cfg.set_with("install_manufacturing_job", |k| {
            k.add_with("contribution_methods", |m| {
                m.set("job_owner_type.any", true)
                    .set("job_location.any", true)
                    .set("item_type.any", true)
            })
        }),
        6 => cfg.set_with("complete_daily_goal", |k| {
            k.add_with("contribution_methods", |m| {
                m.set("daily_goal.any", true)
                    .set("daily_goal_category.category", 1i32)
            })
        }),
        7 => cfg.set_with("damage_ship", |k| {
            k.add_with("contribution_methods", |m| {
                m.set("solar_system.any", true)
                    .set("ship_type.any", true)
                    .set("identity.any", true)
            })
        }),
        _ => cfg.set_with("space_jump", |m| m),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn encode_isk_unit(isk: i64) -> Vec<u8> {
    crate::quasar_gateway::pb_encode_or_log(
        "quasar_daily_goals.encode_isk_unit",
        pb::message("eve_public.dailygoal.Unit").set("isk.amount.units", isk),
    )
    .unwrap_or_default()
}

#[cfg_attr(not(test), allow(dead_code))]
fn encode_sp_unit(sp: u32) -> Vec<u8> {
    crate::quasar_gateway::pb_encode_or_log(
        "quasar_daily_goals.encode_sp_unit",
        pb::message("eve_public.dailygoal.Unit").set("sp.amount", sp),
    )
    .unwrap_or_default()
}

fn build_daily_goal_attributes(
    attrs_type_name: &str,
    tpl: &DailyGoalTemplate,
    active_after_unix: u64,
    active_until_unix: u64,
) -> ProtoBuilder {
    let mut b = pb::message(attrs_type_name)
        .set("name_message.sequential", tpl.effective_name_message_id())
        .set(
            "description_message.sequential",
            tpl.effective_description_message_id(),
        )
        .set(
            "help_text_message.sequential",
            tpl.effective_help_text_message_id(),
        )
        .set(
            "assigner.sequential",
            tpl.effective_assigner_corporation_id(),
        )
        .set("category", tpl.effective_goal_category() as i32)
        .set_with("contribution_configuration", |cfg| {
            fill_contribution_configuration(cfg, tpl.category)
        })
        // Keep career=0 (`CAREER_UNSPECIFIED`) for General daily goals and DailyBonus.
        .set("career", tpl.career.min(4) as i32);

    if tpl.isk_reward > 0 {
        let mut asset_uuid = tpl.uuid;
        asset_uuid[14] = 1;
        b = b.add_with("payment", |p| {
            p.set("asset.uuid", asset_uuid)
                .set("period", 1i32) // PAYMENT_PERIOD_COMPLETION
                .set("unit.isk.amount.units", tpl.isk_reward)
        });
    }
    if tpl.sp_reward > 0.0 {
        let mut asset_uuid = tpl.uuid;
        asset_uuid[14] = 2;
        b = b.add_with("payment", |p| {
            p.set("asset.uuid", asset_uuid)
                .set("period", 1i32) // PAYMENT_PERIOD_COMPLETION
                .set("unit.sp.amount", tpl.sp_reward as u32)
        });
    }
    let effective_em = tpl.effective_evermarks_reward();
    if effective_em > 0 {
        let mut asset_uuid = tpl.uuid;
        asset_uuid[14] = 3;
        b = b.add_with("payment", |p| {
            p.set("asset.uuid", asset_uuid)
                .set("period", 1i32) // PAYMENT_PERIOD_COMPLETION
                .set("unit.lp.amount.amount", effective_em as u64)
                .set(
                    "unit.lp.amount.associated_corporation.sequential",
                    1_000_419u32,
                )
        });
    }

    b.set("target", tpl.target)
        .set(
            "active_after",
            GoogleTimestamp::from_unix(active_after_unix as i64, 0),
        )
        .set(
            "active_until",
            GoogleTimestamp::from_unix(active_until_unix as i64, 0),
        )
}

/// Encodes `eve_public.dailygoal.Attributes` / `eve.dailygoal.Attributes` matching V24 descriptors.
pub fn encode_goal_attributes(tpl: &DailyGoalTemplate, out: &mut Vec<u8>) {
    encode_goal_attributes_with_window(tpl, 39_600, 39_600 + 86_400, out);
}

/// Encodes `eve_public.dailygoal.Attributes` / `eve.dailygoal.Attributes` with explicit active timestamps.
pub fn encode_goal_attributes_for_day_bucket(
    tpl: &DailyGoalTemplate,
    day_bucket: u64,
    out: &mut Vec<u8>,
) {
    let (active_after, active_until) = daily_goal_window_unix(day_bucket);
    encode_goal_attributes_with_window(tpl, active_after, active_until, out);
}

/// Encodes `eve_public.dailygoal.Attributes` / `eve.dailygoal.Attributes` with explicit window timestamps.
pub fn encode_goal_attributes_with_window(
    tpl: &DailyGoalTemplate,
    active_after_unix: u64,
    active_until_unix: u64,
    out: &mut Vec<u8>,
) {
    crate::quasar_gateway::pb_encode_into_or_log(
        "quasar_daily_goals.encode_goal_attributes_with_window",
        build_daily_goal_attributes(
            "eve_public.dailygoal.Attributes",
            tpl,
            active_after_unix,
            active_until_unix,
        ),
        out,
    );
}

fn add_daily_goal_earnings(mut builder: ProtoBuilder, tpl: &DailyGoalTemplate) -> ProtoBuilder {
    if tpl.isk_reward > 0 {
        builder = builder.add_with("earnings", |e| {
            e.set("unit.isk.amount.units", tpl.isk_reward)
        });
    }
    if tpl.sp_reward > 0.0 {
        builder = builder.add_with("earnings", |e| {
            e.set("unit.sp.amount", tpl.sp_reward as u32)
        });
    }
    let effective_em = tpl.effective_evermarks_reward();
    if effective_em > 0 {
        builder = builder.add_with("earnings", |e| {
            e.set("unit.lp.amount.amount", effective_em as u64)
                .set(
                    "unit.lp.amount.associated_corporation.sequential",
                    1_000_419u32,
                )
        });
    }
    builder
}

/// Encodes `repeated eve_public.dailygoal.api.Earning` (one `Earning` per `Unit` oneof variant).
pub fn encode_goal_earnings(tpl: &DailyGoalTemplate, field_num: u32, out: &mut Vec<u8>) {
    let mut fields = Vec::new();
    if tpl.isk_reward > 0 {
        if let Some(bytes) = crate::quasar_gateway::pb_encode_or_log(
            "quasar_daily_goals.encode_goal_earnings.isk",
            pb::message("eve_public.dailygoal.api.Earning")
                .set("unit.isk.amount.units", tpl.isk_reward),
        ) {
            fields.push(WireField {
                field_number: field_num,
                value: WireValue::LengthDelimited(bytes),
            });
        }
    }
    if tpl.sp_reward > 0.0 {
        if let Some(bytes) = crate::quasar_gateway::pb_encode_or_log(
            "quasar_daily_goals.encode_goal_earnings.sp",
            pb::message("eve_public.dailygoal.api.Earning")
                .set("unit.sp.amount", tpl.sp_reward as u32),
        ) {
            fields.push(WireField {
                field_number: field_num,
                value: WireValue::LengthDelimited(bytes),
            });
        }
    }
    let effective_em = tpl.effective_evermarks_reward();
    if effective_em > 0 {
        if let Some(bytes) = crate::quasar_gateway::pb_encode_or_log(
            "quasar_daily_goals.encode_goal_earnings.lp",
            pb::message("eve_public.dailygoal.api.Earning")
                .set("unit.lp.amount.amount", effective_em as u64)
                .set(
                    "unit.lp.amount.associated_corporation.sequential",
                    1_000_419u32,
                ),
        ) {
            fields.push(WireField {
                field_number: field_num,
                value: WireValue::LengthDelimited(bytes),
            });
        }
    }
    if !fields.is_empty() {
        out.extend_from_slice(&WireMessage { fields }.encode());
    }
}

pub fn extract_uuid(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> Option<[u8; 16]> {
    if let Some(DynamicValue::Message(id_msg)) = dyn_msg.get(field_num) {
        if let Some(DynamicValue::Bytes(b)) = id_msg.get(1) {
            if b.len() == 16 {
                let mut arr = [0u8; 16];
                arr.copy_from_slice(b);
                return Some(arr);
            }
        }
    }
    None
}

// ============================================================================
// Handler Registration
// ============================================================================

/// Resolved daily goal payload data for `GetRequest` handlers.
#[derive(Debug, Clone)]
pub struct ResolvedDailyGoal {
    pub template: DailyGoalTemplate,
    pub current_progress: u64,
    pub day_bucket: u64,
    pub is_expired_or_catalog: bool,
}

/// Computes `(active_after_unix, active_until_unix)` for `GetResponse`, ensuring expired
/// or catalog-fallback goals always report `active_until < effective_now` when `effective_now > 0`.
fn resolved_goal_window_unix(resolved: &ResolvedDailyGoal, effective_now: u64) -> (u64, u64) {
    let (after, until) = daily_goal_window_unix(resolved.day_bucket);
    if !resolved.is_expired_or_catalog {
        return (after, until);
    }
    if effective_now == 0 {
        return (0, 0);
    }
    if until < effective_now {
        return (after, until);
    }
    let yesterday_bucket = daily_goal_day_bucket(effective_now).saturating_sub(1);
    let (prev_after, prev_until) = daily_goal_window_unix(yesterday_bucket);
    if prev_until < effective_now {
        (prev_after, prev_until)
    } else {
        let clamped_until = effective_now.saturating_sub(1);
        let clamped_after = clamped_until.saturating_sub(86_400);
        (clamped_after, clamped_until)
    }
}

fn resolve_daily_goal_for_rpc(
    mgr: &DailyGoalManager,
    cid: Option<u32>,
    dyn_msg: &DynamicQuasarMessage,
    now_unix: u64,
) -> Result<(ResolvedDailyGoal, u64), (u32, String)> {
    let Some(cid) = cid else {
        return Err((STATUS_FORBIDDEN, "Active character required".into()));
    };
    let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
        return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
    };
    let effective_now = if now_unix > 0 { now_unix } else { mgr.now_unix() };
    let resolved = mgr
        .resolve_goal_for_get_at(cid, &target_uuid, effective_now)
        .ok_or_else(|| (STATUS_NOT_FOUND, "Daily goal not found".into()))?;
    Ok((resolved, effective_now))
}

pub fn register_daily_goal_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    manager: Arc<DailyGoalManager>,
) {
    // 1. eve_public.dailygoal.api.GetAllCurrentRequest -> GetAllCurrentResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.dailygoal.api.GetAllCurrentRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
            let mut out = pb::message("eve_public.dailygoal.api.GetAllCurrentResponse");
            for g in &goals {
                let (after, until) = daily_goal_window_unix(g.day_bucket);
                let attrs = build_daily_goal_attributes(
                    "eve_public.dailygoal.Attributes",
                    &g.template,
                    after,
                    until,
                );
                out = out.add_with("goals", |ge| {
                    add_daily_goal_earnings(
                        ge.set("id.uuid", g.uuid)
                            .set("goal", attrs)
                            .set("current_progress", g.current_progress),
                        &g.template,
                    )
                    .when(g.paid_completion, |ge| ge.set("paid_completion", true))
                });
            }
            out.to_any_or_internal_err()
        }),
    );

    // 2. eve_public.dailygoal.api.GetRequest -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.dailygoal.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let (resolved, effective_now) = resolve_daily_goal_for_rpc(
                &mgr,
                ctx.effective_character_id(),
                dyn_msg,
                ctx.now_unix(),
            )?;
            let (after, until) = resolved_goal_window_unix(&resolved, effective_now);
            let attrs = build_daily_goal_attributes(
                "eve_public.dailygoal.Attributes",
                &resolved.template,
                after,
                until,
            );
            let resp = pb::message("eve_public.dailygoal.api.GetResponse")
                .set("goal", attrs)
                .set("progress", resolved.current_progress);
            // Omit earnings for expired/catalog-only goals so client `has_unclaimed_rewards()` stays false.
            if resolved.is_expired_or_catalog {
                resp
            } else {
                add_daily_goal_earnings(resp, &resolved.template)
            }
            .to_any_or_internal_err()
        }),
    );

    // 3. eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest -> GetAllCompletedWithEntitlementsResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
            let mut out =
                pb::message("eve_public.dailygoal.api.GetAllCompletedWithEntitlementsResponse");
            for g in goals.iter().filter(|g| g.completed) {
                out = out.add_with("goals", |ge| ge.set("id.uuid", g.uuid));
            }
            out.to_any_or_internal_err()
        }),
    );

    // 4. eve_public.dailygoal.api.GetAllWithRewardsRequest -> GetAllWithRewardsResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.dailygoal.api.GetAllWithRewardsRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
            let mut out = pb::message("eve_public.dailygoal.api.GetAllWithRewardsResponse");
            for g in goals.iter().filter(|g| g.completed && !g.claimed) {
                out = out.add_with("ids", |id| id.set("uuid", g.uuid));
            }
            out.to_any_or_internal_err()
        }),
    );

    // 5. eve_public.dailygoal.api.RedeemRequest -> RedeemResponse
    let mgr = Arc::clone(&manager);
    let db_redeem = Arc::clone(&db);
    let q_redeem = Arc::clone(&router.queued_notices);
    router.register_handler(
        "eve_public.dailygoal.api.RedeemRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let redeemed = mgr.redeem_goal(cid, &target_uuid, ctx.now_unix(), &db_redeem)?;
            mgr.save_to_db();

            // Publish RedeemedNotice
            crate::quasar_gateway::queue_pb_notice(
                &q_redeem,
                NoticeScope::Character(cid),
                "quasar_daily_goals.redeem.redeemed",
                pb::message("eve_public.dailygoal.api.RedeemedNotice")
                    .set("goal.uuid", redeemed.uuid),
                ctx.now_unix(),
            );

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.RedeemResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 6. eve_public.dailygoal.api.RedeemAllRequest -> RedeemAllResponse
    let mgr = Arc::clone(&manager);
    let db_redeem_all = Arc::clone(&db);
    let q_redeem_all = Arc::clone(&router.queued_notices);
    router.register_handler(
        "eve_public.dailygoal.api.RedeemAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
            for g in goals.iter().filter(|g| g.completed && !g.claimed) {
                if let Ok(redeemed) = mgr.redeem_goal(cid, &g.uuid, ctx.now_unix(), &db_redeem_all) {
                    crate::quasar_gateway::queue_pb_notice(
                        &q_redeem_all,
                        NoticeScope::Character(cid),
                        "quasar_daily_goals.redeem_all.redeemed",
                        pb::message("eve_public.dailygoal.api.RedeemedNotice")
                            .set("goal.uuid", redeemed.uuid),
                        ctx.now_unix(),
                    );
                }
            }
            mgr.save_to_db();
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.RedeemAllResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 7. eve_public.dailygoal.api.PayForCompletionRequest -> PayForCompletionResponse
    let mgr = Arc::clone(&manager);
    let db_pay = Arc::clone(&db);
    let q_pay = Arc::clone(&router.queued_notices);
    router.register_handler(
        "eve_public.dailygoal.api.PayForCompletionRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let goal = mgr.pay_for_completion(cid, &target_uuid, ctx.now_unix(), &db_pay)?;
            mgr.save_to_db();

            // 1. ProgressedNotice
            crate::quasar_gateway::queue_pb_notice(
                &q_pay,
                NoticeScope::Character(cid),
                "quasar_daily_goals.pay_for_completion.progressed",
                pb::message("eve_public.dailygoal.api.ProgressedNotice")
                    .set("goal.uuid", goal.uuid)
                    .set("current_progress", goal.current_progress),
                ctx.now_unix(),
            );

            // 2. CompletedNotice
            crate::quasar_gateway::queue_pb_notice(
                &q_pay,
                NoticeScope::Character(cid),
                "quasar_daily_goals.pay_for_completion.completed",
                pb::message("eve_public.dailygoal.api.CompletedNotice")
                    .set("goal.uuid", goal.uuid),
                ctx.now_unix(),
            );

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.PayForCompletionResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 8. eve_public.dailygoal.api.admin.ProgressRequest -> ProgressResponse
    let mgr = Arc::clone(&manager);
    let q_prog = Arc::clone(&router.queued_notices);
    router.register_handler(
        "eve_public.dailygoal.api.admin.ProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let target_char = dyn_msg
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::Message(m) => m.get(1).and_then(|seq| match seq {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Uint64(u) => Some(*u as u32),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Int64(i) => Some(*i as u32),
                        _ => None,
                    }),
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Uint64(u) => Some(*u as u32),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Int64(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(cid);
            let delta = dyn_msg
                .get(3)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(1);
            let updated = mgr.add_progress(target_char, &target_uuid, delta, ctx.now_unix());
            mgr.save_to_db();

            if let Some((goal, just_completed)) = updated {
                crate::quasar_gateway::queue_pb_notice(
                    &q_prog,
                    NoticeScope::Character(target_char),
                    "quasar_daily_goals.admin_progress.progressed",
                    pb::message("eve_public.dailygoal.api.ProgressedNotice")
                        .set("goal.uuid", target_uuid)
                        .set("current_progress", goal.current_progress),
                    ctx.now_unix(),
                );

                if just_completed {
                    crate::quasar_gateway::queue_pb_notice(
                        &q_prog,
                        NoticeScope::Character(target_char),
                        "quasar_daily_goals.admin_progress.completed",
                        pb::message("eve_public.dailygoal.api.CompletedNotice")
                            .set("goal.uuid", target_uuid),
                        ctx.now_unix(),
                    );
                }
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.admin.ProgressResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 9. eve.dailygoal.api.GetRequest -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.dailygoal.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let (resolved, effective_now) = resolve_daily_goal_for_rpc(
                &mgr,
                ctx.effective_character_id(),
                dyn_msg,
                ctx.now_unix(),
            )?;
            let (after, until) = resolved_goal_window_unix(&resolved, effective_now);
            let attrs = build_daily_goal_attributes(
                "eve.dailygoal.Attributes",
                &resolved.template,
                after,
                until,
            );
            pb::message("eve.dailygoal.api.GetResponse")
                .set("goal", attrs)
                .to_any_or_internal_err()
        }),
    );

    // 10. eve.dailygoal.api.GetAllRequest -> GetAllResponse (repeated eve.dailygoal.Identifier goals = 1)
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.dailygoal.api.GetAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
            let mut out = pb::message("eve.dailygoal.api.GetAllResponse");
            for g in &goals {
                out = out.add_with("goals", |id| id.set("uuid", g.uuid));
            }
            out.to_any_or_internal_err()
        }),
    );

    // 11. eve.dailygoal.api.GetProgressRequest -> GetProgressResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.dailygoal.api.GetProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
            let g = goals
                .iter()
                .find(|g| g.uuid == target_uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Daily goal not found".into()))?;

            add_daily_goal_earnings(
                pb::message("eve.dailygoal.api.GetProgressResponse")
                    .set("current", g.current_progress)
                    .when(g.completed, |b| {
                        let ts = GoogleTimestamp::from_unix(
                            g.completed_at_unix.unwrap_or(ctx.now_unix()) as i64,
                            0,
                        );
                        b.set("completed", ts)
                    })
                    .when(!g.completed, |b| b.set("not_completed", true)),
                &g.template,
            )
            .to_any_or_internal_err()
        }),
    );

    super::quasar_daily_goals_admin::register(router, &manager, &db);
}

#[cfg(test)] #[path = "quasar_daily_goals_tests.rs"] mod quasar_daily_goals_tests;
#[cfg(test)] #[path = "quasar_daily_goals_edges_tests.rs"] mod quasar_daily_goals_edges_tests;

