//! `quasar_progression` — Master Registrar for Quasar Character & Progression Track (`Q-4`).
//!
//! Ties together:
//! - `quasar_daily_goals`: Daily goals rotation, progress, redemption, notices.
//! - `quasar_career_goals`: AIR Career Program paths, goals, points, rewards.
//! - `quasar_achievements`: AIR Achievements, categories, milestones, score tracking.
//! - `quasar_skills_clones`: Personal & corp skill plans, extractors, injectors, skill types.
//! - `quasar_progression_extra`: Community tracks, narrative tales, generic goals, titles, campaigns.

#![forbid(unsafe_code)]

use crate::quasar_gateway::PublicGatewayRouter;
use crate::services::quasar_achievements::{register_achievement_handlers, AchievementManager};
use crate::services::quasar_career_goals::{register_career_goal_handlers, CareerGoalManager};
use crate::services::quasar_daily_goals::{register_daily_goal_handlers, DailyGoalManager};
use crate::services::quasar_progression_extra::{
    register_progression_extra_handlers, ProgressionExtraManager,
};
use crate::services::quasar_skills_clones::{register_skill_handlers, SkillPlanManager};
use crate::services::quasar_social_corp::QuasarCorpState;
use eve_db::WorldRepository;
use std::sync::Arc;

pub struct ProgressionManagers {
    pub daily_goals: Arc<DailyGoalManager>,
    pub career_goals: Arc<CareerGoalManager>,
    pub achievements: Arc<AchievementManager>,
    pub skill_plans: Arc<SkillPlanManager>,
    pub extra: Arc<ProgressionExtraManager>,
    pub corp_state: Arc<QuasarCorpState>,
}

impl Default for ProgressionManagers {
    fn default() -> Self {
        let corp_state = Arc::new(QuasarCorpState::default());
        Self {
            daily_goals: Arc::new(DailyGoalManager::new(None)),
            career_goals: Arc::new(CareerGoalManager::new(None)),
            achievements: Arc::new(AchievementManager::new(None)),
            skill_plans: Arc::new(SkillPlanManager::new()),
            extra: Arc::new(ProgressionExtraManager::new_with_sde(None, None, Some(Arc::clone(&corp_state)))),
            corp_state,
        }
    }
}

pub fn new_progression_managers(
    db: Arc<WorldRepository>,
    sde: Arc<eve_sde::SdeRegistry>,
) -> Arc<ProgressionManagers> {
    let corp_state = QuasarCorpState::from_db(Arc::clone(&db));
    Arc::new(ProgressionManagers {
        daily_goals: Arc::new(DailyGoalManager::new(Some(Arc::clone(&db)))),
        career_goals: Arc::new(CareerGoalManager::new(Some(Arc::clone(&db)))),
        achievements: Arc::new(AchievementManager::new(Some(Arc::clone(&db)))),
        skill_plans: Arc::new(SkillPlanManager::new_with_db(Some(Arc::clone(&db)))),
        extra: Arc::new(ProgressionExtraManager::new_with_sde(
            Some(Arc::clone(&db)),
            Some(Arc::clone(&sde)),
            Some(Arc::clone(&corp_state)),
        )),
        corp_state,
    })
}

pub fn register_all_progression_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    managers: Arc<ProgressionManagers>,
) {
    register_daily_goal_handlers(router, Arc::clone(&db), Arc::clone(&managers.daily_goals));
    register_career_goal_handlers(router, Arc::clone(&db), Arc::clone(&managers.career_goals));
    register_achievement_handlers(router, Arc::clone(&db), Arc::clone(&managers.achievements));
    register_skill_handlers(router, Arc::clone(&db), Arc::clone(&managers.skill_plans));
    register_progression_extra_handlers(router, Arc::clone(&db), Arc::clone(&managers.extra));
}
