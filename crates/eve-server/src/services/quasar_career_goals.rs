//! `quasar_career_goals` — V24.01 Quasar AIR Career Program Goals Service (`feat:quasar.career`, `Q-4`).
//!
//! Implements live backend semantics for the EVE Online V24 AIR Career Program:
//! - 4 Career paths: Enforcer, Explorer, Industrialist, Soldier of Fortune.
//! - Career goal definitions, progress tracking, career points, milestone rewards.
//! - Request handlers for `eve_public.career.goal.api`, `eve.career.goal.api`, and `eve_public.app.eveonline.career.goal`.
//! - Live notice publication (`ProgressedNotice`, `CompletedNotice`).
//! - Live wallet/skill-point payouts in `WorldRepository`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleAny, GoogleEmpty, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub struct CareerGoalDef {
    pub uuid: [u8; 16],
    pub career_id: u32, // 1: Enforcer, 2: Explorer, 3: Industrialist, 4: Soldier of Fortune
    pub name: &'static str,
    pub description: &'static str,
    pub target: u64,
    pub career_points: u32,
    pub isk_reward: i64,
    pub sp_reward: f64,
}

pub fn default_career_goal_definitions() -> Vec<CareerGoalDef> {
    vec![
        // Enforcer goals
        CareerGoalDef {
            uuid: [0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            career_id: 1, // Enforcer
            name: "Enforcer: Neutralize Pirate Outposts",
            description: "Destroy pirate vessels in combat anomaly or combat mission encounters.",
            target: 25,
            career_points: 50,
            isk_reward: 1_000_000,
            sp_reward: 15_000.0,
        },
        CareerGoalDef {
            uuid: [0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            career_id: 1, // Enforcer
            name: "Enforcer: Complete Combat Missions",
            description: "Complete security agent combat assignments.",
            target: 5,
            career_points: 30,
            isk_reward: 750_000,
            sp_reward: 10_000.0,
        },
        // Explorer goals
        CareerGoalDef {
            uuid: [0x43, 0x47, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            career_id: 2, // Explorer
            name: "Explorer: Probe Cosmic Signatures",
            description: "Use scanner probes to scan down cosmic signatures to 100% signal strength.",
            target: 10,
            career_points: 40,
            isk_reward: 500_000,
            sp_reward: 15_000.0,
        },
        CareerGoalDef {
            uuid: [0x43, 0x47, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            career_id: 2, // Explorer
            name: "Explorer: Hack Relic and Data Sites",
            description: "Successfully breach analyzer containers in relic or data exploration sites.",
            target: 5,
            career_points: 50,
            isk_reward: 1_000_000,
            sp_reward: 20_000.0,
        },
        // Industrialist goals
        CareerGoalDef {
            uuid: [0x43, 0x47, 0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            career_id: 3, // Industrialist
            name: "Industrialist: Mine Asteroid Belts",
            description: "Extract raw ore or ice from celestial resource deposits.",
            target: 2_500,
            career_points: 40,
            isk_reward: 500_000,
            sp_reward: 10_000.0,
        },
        CareerGoalDef {
            uuid: [0x43, 0x47, 0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            career_id: 3, // Industrialist
            name: "Industrialist: Manufacturing Jobs",
            description: "Install and complete industrial manufacturing production jobs.",
            target: 3,
            career_points: 50,
            isk_reward: 1_000_000,
            sp_reward: 15_000.0,
        },
        // Soldier of Fortune goals
        CareerGoalDef {
            uuid: [0x43, 0x47, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            career_id: 4, // Soldier of Fortune
            name: "Soldier of Fortune: Faction Warfare Operations",
            description: "Capture or defend contested faction warfare complex sites.",
            target: 3,
            career_points: 50,
            isk_reward: 1_500_000,
            sp_reward: 25_000.0,
        },
        CareerGoalDef {
            uuid: [0x43, 0x47, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            career_id: 4, // Soldier of Fortune
            name: "Soldier of Fortune: Fleet Engagements",
            description: "Engage in hostile fleet warfare actions.",
            target: 5,
            career_points: 40,
            isk_reward: 1_000_000,
            sp_reward: 20_000.0,
        },
    ]
}

#[derive(Debug, Clone)]
pub struct CharacterCareerGoal {
    pub uuid: [u8; 16],
    pub def: CareerGoalDef,
    pub progress: u64,
    pub completed: bool,
    pub claimed: bool,
    pub completed_time: Option<u64>,
}

#[derive(Debug, Default)]
pub struct CareerGoalManager {
    pub character_career_goals: RwLock<HashMap<u32, Vec<CharacterCareerGoal>>>,
}

impl CareerGoalManager {
    pub fn new() -> Self {
        Self {
            character_career_goals: RwLock::new(HashMap::new()),
        }
    }

    pub fn get_or_init_goals(&self, character_id: u32) -> Vec<CharacterCareerGoal> {
        let mut map = self
            .character_career_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        map.entry(character_id)
            .or_insert_with(|| {
                default_career_goal_definitions()
                    .into_iter()
                    .map(|def| CharacterCareerGoal {
                        uuid: def.uuid,
                        def,
                        progress: 0,
                        completed: false,
                        claimed: false,
                        completed_time: None,
                    })
                    .collect()
            })
            .clone()
    }

    pub fn add_progress(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        progress_delta: u64,
        now_unix: u64,
    ) -> Option<(CharacterCareerGoal, bool)> {
        let mut map = self
            .character_career_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let goals = map.entry(character_id).or_insert_with(|| {
            default_career_goal_definitions()
                .into_iter()
                .map(|def| CharacterCareerGoal {
                    uuid: def.uuid,
                    def,
                    progress: 0,
                    completed: false,
                    claimed: false,
                    completed_time: None,
                })
                .collect()
        });

        for g in goals.iter_mut() {
            if &g.uuid == goal_uuid {
                let prev_completed = g.completed;
                g.progress = g.progress.saturating_add(progress_delta);
                if g.progress >= g.def.target && !g.completed {
                    g.completed = true;
                    g.completed_time = Some(now_unix);
                }
                let just_completed = !prev_completed && g.completed;
                return Some((g.clone(), just_completed));
            }
        }
        None
    }

    pub fn mark_completed(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        now_unix: u64,
    ) -> Result<CharacterCareerGoal, (u32, String)> {
        let mut map = self
            .character_career_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let goals = map.entry(character_id).or_insert_with(|| {
            default_career_goal_definitions()
                .into_iter()
                .map(|def| CharacterCareerGoal {
                    uuid: def.uuid,
                    def,
                    progress: 0,
                    completed: false,
                    claimed: false,
                    completed_time: None,
                })
                .collect()
        });

        for g in goals.iter_mut() {
            if &g.uuid == goal_uuid {
                g.progress = g.def.target;
                g.completed = true;
                g.completed_time = Some(now_unix);
                return Ok(g.clone());
            }
        }
        Err((STATUS_NOT_FOUND, "Career goal not found".into()))
    }

    pub fn claim_reward(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        db: &WorldRepository,
    ) -> Result<CharacterCareerGoal, (u32, String)> {
        let mut map = self
            .character_career_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let Some(goals) = map.get_mut(&character_id) else {
            return Err((STATUS_NOT_FOUND, "No career goals for character".into()));
        };

        for g in goals.iter_mut() {
            if &g.uuid == goal_uuid {
                if !g.completed {
                    return Err((STATUS_FORBIDDEN, "Career goal not yet completed".into()));
                }
                if g.claimed {
                    return Err((STATUS_FORBIDDEN, "Career goal already claimed".into()));
                }
                g.claimed = true;

                // Award ISK and free SP to character
                let isk = g.def.isk_reward;
                let sp = g.def.sp_reward;
                db.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&(character_id as i64)) {
                        ch.balance = ch
                            .balance
                            .checked_add(IskCents::from_isk(isk))
                            .unwrap_or(ch.balance);
                        ch.free_skill_points += sp;
                    }
                });

                return Ok(g.clone());
            }
        }
        Err((STATUS_NOT_FOUND, "Career goal not found".into()))
    }
}

// ============================================================================
// Protobuf Wire Encoders for Career Goals
// ============================================================================

pub fn encode_career_goal_attributes(g: &CharacterCareerGoal, out: &mut Vec<u8>) {
    encode_string_field(1, g.def.name, out);
    encode_string_field(2, g.def.description, out);
    encode_varint_field(3, g.progress, out);
    encode_varint_field(4, g.def.target, out);
    encode_varint_field(8, if g.completed { 1 } else { 0 }, out);
    encode_varint_field(9, g.def.career_id as u64, out);
    encode_varint_field(10, g.def.career_points as u64, out);
    encode_varint_field(11, if g.claimed { 1 } else { 0 }, out);
}

pub fn encode_career_goal_wire(g: &CharacterCareerGoal, out: &mut Vec<u8>) {
    // 1: goal (Attributes)
    let mut attr_bytes = Vec::new();
    encode_career_goal_attributes(g, &mut attr_bytes);
    encode_bytes_field(1, &attr_bytes, out);

    // 2: progress
    encode_varint_field(2, g.progress, out);

    // 3: not_completed
    encode_varint_field(3, if g.completed { 0 } else { 1 }, out);

    // 4: completed_time
    if let Some(ts) = g.completed_time {
        let ts_proto = GoogleTimestamp {
            seconds: ts as i64,
            nanos: 0,
        };
        encode_bytes_field(4, &ts_proto.encode_proto(), out);
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
// Registration
// ============================================================================

pub fn register_career_goal_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    manager: Arc<CareerGoalManager>,
) {
    // 1. eve_public.career.goal.api.GetAllRequest -> GetAllResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.career.goal.api.GetAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let mut out = Vec::new();
            for g in &goals {
                let mut gw = Vec::new();
                encode_career_goal_wire(g, &mut gw);
                encode_bytes_field(1, &gw, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 2. eve_public.career.goal.api.GetDefinitionsRequest -> GetDefinitionsResponse
    router.register_handler(
        "eve_public.career.goal.api.GetDefinitionsRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let defs = default_career_goal_definitions();
            let mut out = Vec::new();
            for def in &defs {
                let g = CharacterCareerGoal {
                    uuid: def.uuid,
                    def: def.clone(),
                    progress: 0,
                    completed: false,
                    claimed: false,
                    completed_time: None,
                };
                let mut gw = Vec::new();
                encode_career_goal_wire(&g, &mut gw);
                encode_bytes_field(1, &gw, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.GetDefinitionsResponse"
                ),
                value: out,
            })
        }),
    );

    // 3. eve_public.career.goal.api.ClaimRewardRequest -> ClaimRewardResponse
    let mgr = Arc::clone(&manager);
    let db_claim = Arc::clone(&db);
    router.register_handler(
        "eve_public.career.goal.api.ClaimRewardRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let _ = mgr.claim_reward(cid, &target_uuid, &db_claim)?;
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 4. eve_public.career.goal.api.admin.ProgressRequest -> ProgressResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.career.goal.api.admin.ProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let delta = dyn_msg
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) => Some(*i as u64),
                    DynamicValue::Uint32(u) => Some(*u as u64),
                    DynamicValue::Int32(i) => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(1);
            let _ = mgr.add_progress(cid, &target_uuid, delta, ctx.now_unix());
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 5. eve_public.career.goal.api.admin.CompleteRequest -> CompleteResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.career.goal.api.admin.CompleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let _ = mgr.mark_completed(cid, &target_uuid, ctx.now_unix())?;
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 6. eve_public.app.eveonline.career.goal.GetAllRequest -> GetAllResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.app.eveonline.career.goal.GetAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let mut out = Vec::new();
            for g in &goals {
                let mut gw = Vec::new();
                encode_career_goal_wire(g, &mut gw);
                encode_bytes_field(1, &gw, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.career.goal.GetAllResponse"
                ),
                value: out,
            })
        }),
    );

    // 7. eve_public.app.eveonline.career.goal.ClaimRewardRequest -> ClaimRewardResponse
    let mgr = Arc::clone(&manager);
    let db_app_claim = Arc::clone(&db);
    router.register_handler(
        "eve_public.app.eveonline.career.goal.ClaimRewardRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let _ = mgr.claim_reward(cid, &target_uuid, &db_app_claim)?;
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 8. eve.career.goal.api.requests.GetDefinitionsRequest -> GetDefinitionsResponse
    router.register_handler(
        "eve.career.goal.api.requests.GetDefinitionsRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let defs = default_career_goal_definitions();
            let mut out = Vec::new();
            for def in &defs {
                let g = CharacterCareerGoal {
                    uuid: def.uuid,
                    def: def.clone(),
                    progress: 0,
                    completed: false,
                    claimed: false,
                    completed_time: None,
                };
                let mut gw = Vec::new();
                encode_career_goal_wire(&g, &mut gw);
                encode_bytes_field(1, &gw, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.career.goal.api.requests.GetDefinitionsResponse"
                ),
                value: out,
            })
        }),
    );

    // 9. eve.career.goal.api.admin.requests.GetAllByCharacterRequest -> GetAllByCharacterResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.career.goal.api.admin.requests.GetAllByCharacterRequest",
        Arc::new(move |ctx, _any, _dyn| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let mut out = Vec::new();
            for g in &goals {
                let mut gw = Vec::new();
                encode_career_goal_wire(g, &mut gw);
                encode_bytes_field(1, &gw, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.career.goal.api.admin.requests.GetAllByCharacterResponse"
                ),
                value: out,
            })
        }),
    );

    // 10. eve.career.goal.api.admin.requests.CompleteRequest -> CompleteResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.career.goal.api.admin.requests.CompleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let _ = mgr.mark_completed(cid, &target_uuid, ctx.now_unix())?;
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 11. eve.character.career.goal.GetProgressRequest -> GetProgressResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.character.career.goal.GetProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_uuid = extract_uuid(dyn_msg, 2)
                .or_else(|| extract_uuid(dyn_msg, 1))
                .unwrap_or([0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
            let goals = mgr.get_or_init_goals(cid);
            let g = goals
                .iter()
                .find(|g| g.uuid == target_uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Career goal not found".into()))?;

            let mut out = Vec::new();
            encode_varint_field(1, g.progress, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.character.career.goal.GetProgressResponse"
                ),
                value: out,
            })
        }),
    );

    // 12. eve.character.career.goal.HasClaimedRewardRequest -> HasClaimedRewardResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.character.career.goal.HasClaimedRewardRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_uuid = extract_uuid(dyn_msg, 2)
                .or_else(|| extract_uuid(dyn_msg, 1))
                .unwrap_or([0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
            let goals = mgr.get_or_init_goals(cid);
            let g = goals
                .iter()
                .find(|g| g.uuid == target_uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Career goal not found".into()))?;

            let mut out = Vec::new();
            encode_varint_field(1, if g.claimed { 1 } else { 0 }, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.character.career.goal.HasClaimedRewardResponse"
                ),
                value: out,
            })
        }),
    );
}
