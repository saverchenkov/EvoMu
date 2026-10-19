//! `quasar_services` — Quasar Protobuf Backend Services (`Q-3` Identity & Entitlements,
//! `Q-4` Character Progression, `Q-5` Fitting Simulation, `Q-6` Market & Wallet).

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_dogma::{ModuleState, ShipFittingEngine};
use eve_proto::quasar::*;
use eve_sde::{attr_ids, SdeRegistry, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

/// Registers all `Q-3`, `Q-4`, `Q-5`, and `Q-6` Quasar services on `router`.
pub fn register_all_quasar_services(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    register_q3_identity_entitlement_services(router, Arc::clone(&db));
    register_q4_character_progression_services(router, Arc::clone(&db));
    register_q5_fitting_simulation_services(router, Arc::clone(&db), Arc::clone(&sde));
    register_q6_market_wallet_services(router, Arc::clone(&db), Arc::clone(&sde));
    register_q7_space_and_combat_services(router, Arc::clone(&db), Arc::clone(&sde));
    register_q8_sovereignty_and_events_services(router, Arc::clone(&db), Arc::clone(&sde));
    register_q9_cosmetics_and_client_services(router, db, sde);
}

// ============================================================================
// Q-3: Identity & Entitlements Services (`eve_public.identity.v1.*`, `eve_public.account.v1.*`)
// ============================================================================

pub fn register_q3_identity_entitlement_services(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
) {
    // 1. eve_public.identity.v1.GetEntitlementsRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            GetEntitlementsRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetEntitlementsRequest::decode_proto(&any.value).unwrap_or_default();
                let uid = ctx
                    .effective_user_id()
                    .map(|u| u as u64)
                    .or(req.user_id)
                    .ok_or_else(|| (STATUS_UNAUTHORIZED, "User authentication required".into()))?;

                let now_unix = ctx
                    .received
                    .map(|t| t.seconds.max(0) as u64)
                    .unwrap_or(1_760_000_000);

                let (is_omega, omega_expires) = db_c.with_snapshot(|snap| {
                    let has_omega_char = snap
                        .characters
                        .values()
                        .any(|c| c.user_id == uid && c.clone_grade == 2);
                    if has_omega_char {
                        (true, now_unix + 30 * 86_400)
                    } else {
                        (true, now_unix + 365 * 86_400) // Default active Omega for emulator testing
                    }
                });

                let entitlements = if is_omega {
                    vec![
                        "eve.omega.clone_subscription".to_string(),
                        "eve.market.unrestricted".to_string(),
                        "eve.chat.unrestricted".to_string(),
                        "eve.corporation.create".to_string(),
                        "eve.skill.unrestricted".to_string(),
                    ]
                } else {
                    vec![
                        "eve.alpha.clone_state".to_string(),
                        "eve.chat.unrestricted".to_string(),
                    ]
                };

                let resp = GetEntitlementsResponse {
                    user_id: uid,
                    is_omega,
                    omega_expires_at_unix: omega_expires,
                    entitlements,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 2. eve_public.identity.v1.GetIdentityRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            GetIdentityRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetIdentityRequest::decode_proto(&any.value).unwrap_or_default();
                let uid = ctx
                    .effective_user_id()
                    .map(|u| u as u64)
                    .or(req.user_id)
                    .ok_or_else(|| (STATUS_UNAUTHORIZED, "User authentication required".into()))?;

                let (username, plex_bal) = db_c.with_snapshot(|snap| {
                    let uname = snap
                        .accounts
                        .values()
                        .find(|a| a.user_id == uid)
                        .map(|a| a.username.clone())
                        .unwrap_or_else(|| format!("pilot_{uid}"));
                    let plex = snap
                        .characters
                        .values()
                        .find(|c| c.user_id == uid)
                        .map(|c| c.plex_balance)
                        .unwrap_or(500);
                    (uname, plex)
                });

                let resp = GetIdentityResponse {
                    user_id: uid,
                    username,
                    is_omega: true,
                    plex_balance: plex_bal,
                    created_at_unix: 1_700_000_000,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 3. eve_public.account.v1.GetAccountRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            GetAccountRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetAccountRequest::decode_proto(&any.value).unwrap_or_default();
                let uid = ctx
                    .effective_user_id()
                    .map(|u| u as u64)
                    .or(req.user_id)
                    .ok_or_else(|| (STATUS_UNAUTHORIZED, "User authentication required".into()))?;

                let (username, plex_bal) = db_c.with_snapshot(|snap| {
                    let uname = snap
                        .accounts
                        .values()
                        .find(|a| a.user_id == uid)
                        .map(|a| a.username.clone())
                        .unwrap_or_else(|| format!("pilot_{uid}"));
                    let plex = snap
                        .characters
                        .values()
                        .find(|c| c.user_id == uid)
                        .map(|c| c.plex_balance)
                        .unwrap_or(500);
                    (uname, plex)
                });

                let resp = GetAccountResponse {
                    user_id: uid,
                    username,
                    is_omega: true,
                    omega_expires_at_unix: 1_760_000_000 + 30 * 86_400,
                    plex_balance: plex_bal,
                    entitlements: vec![
                        "eve.omega.clone_subscription".to_string(),
                        "eve.market.unrestricted".to_string(),
                    ],
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 4. eve_public.account.v1.GetOmegaStatusRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            GetOmegaStatusRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetOmegaStatusRequest::decode_proto(&any.value).unwrap_or_default();
                let uid = ctx
                    .effective_user_id()
                    .map(|u| u as u64)
                    .or(req.user_id)
                    .unwrap_or(1);

                let is_omega = db_c.with_snapshot(|snap| {
                    snap.characters
                        .values()
                        .find(|c| c.user_id == uid)
                        .map(|c| c.clone_grade == 2)
                        .unwrap_or(true)
                });

                let now_unix = ctx
                    .received
                    .map(|t| t.seconds.max(0) as u64)
                    .unwrap_or(1_760_000_000);

                let resp = GetOmegaStatusResponse {
                    is_omega,
                    omega_expires_at_unix: if is_omega { now_unix + 30 * 86_400 } else { 0 },
                    status_label: if is_omega {
                        "Omega Clone State".to_string()
                    } else {
                        "Alpha Clone State".to_string()
                    },
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 5. eve_public.account.v1.GetPlexBalanceRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            GetPlexBalanceRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetPlexBalanceRequest::decode_proto(&any.value).unwrap_or_default();
                let uid = ctx
                    .effective_user_id()
                    .map(|u| u as u64)
                    .or(req.user_id)
                    .ok_or_else(|| (STATUS_UNAUTHORIZED, "User authentication required".into()))?;

                let plex_bal = db_c.with_snapshot(|snap| {
                    snap.characters
                        .values()
                        .find(|c| c.user_id == uid)
                        .map(|c| c.plex_balance)
                        .unwrap_or(500)
                });

                let resp = GetPlexBalanceResponse {
                    user_id: uid,
                    plex_balance: plex_bal,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // Fallback prefix wildcard handlers for identity & account v1 envelopes
    router.register_handler(
        "eve_public.identity.v1.*",
        Arc::new(|ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let resp = GetIdentityResponse {
                user_id: uid,
                username: format!("pilot_{uid}"),
                is_omega: true,
                plex_balance: 500,
                created_at_unix: 1_700_000_000,
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.account.v1.*",
        Arc::new(|ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let resp = GetAccountResponse {
                user_id: uid,
                username: format!("pilot_{uid}"),
                is_omega: true,
                omega_expires_at_unix: 1_760_000_000 + 30 * 86_400,
                plex_balance: 500,
                entitlements: vec!["eve.omega.clone_subscription".to_string()],
            };
            Ok(resp.to_any())
        }),
    );
}

// ============================================================================
// Q-4: Character Progression Services (`eve_public.character.v1.*`)
// ============================================================================

pub fn register_q4_character_progression_services(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
) {
    // 1. eve_public.character.v1.GetProgressionRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            GetProgressionRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetProgressionRequest::decode_proto(&any.value).unwrap_or_default();
                let cid = ctx
                    .effective_character_id()
                    .or(req.character_id)
                    .ok_or_else(|| (STATUS_FORBIDDEN, "Active character required".into()))?;

                let (total_sp, unallocated_sp, skills_count, career_points, attrs) =
                    db_c.with_snapshot(|snap| {
                        let ch_opt = snap.characters.get(&(cid as i64));
                        let skills_opt = snap.skills.get(&(cid as i64));

                        let trained_sp: u64 = skills_opt
                            .map(|m| m.values().map(|s| s.skill_points as u64).sum())
                            .unwrap_or(0);
                        let char_sp = ch_opt.map(|c| c.skill_points as u64).unwrap_or(0);
                        let free_sp = ch_opt.map(|c| c.free_skill_points as u64).unwrap_or(0);
                        let total = if trained_sp > 0 { trained_sp + free_sp } else { char_sp + free_sp };
                        let count = skills_opt.map(|m| m.len() as u32).unwrap_or(0);
                        let count = if count > 0 { count } else { ch_opt.map(|_| 16).unwrap_or(0) };

                        let neural = ch_opt
                            .map(|c| CharacterAttributes {
                                intelligence: c.neural.intelligence as f64,
                                memory: c.neural.memory as f64,
                                charisma: c.neural.charisma as f64,
                                perception: c.neural.perception as f64,
                                willpower: c.neural.willpower as f64,
                            })
                            .unwrap_or(CharacterAttributes {
                                intelligence: 20.0,
                                memory: 20.0,
                                charisma: 19.0,
                                perception: 20.0,
                                willpower: 20.0,
                            });

                        (total, free_sp as u32, count, 50u32, neural)
                    });

                let resp = GetProgressionResponse {
                    character_id: cid,
                    total_sp,
                    unallocated_sp,
                    skills_count,
                    career_points,
                    attributes: Some(attrs),
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 2. eve_public.character.v1.GetDailyOpportunitiesRequest
    {
        router.register_handler(
            GetDailyOpportunitiesRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req =
                    GetDailyOpportunitiesRequest::decode_proto(&any.value).unwrap_or_default();
                let cid = ctx
                    .effective_character_id()
                    .or(req.character_id)
                    .unwrap_or(90_000_001);

                let opportunities = vec![
                    DailyOpportunity {
                        id: "daily_enforcer_01".to_string(),
                        name: "Destroy NPC Pirates".to_string(),
                        description: "Destroy 5 hostile pirate ships in asteroid belts or combat anomalies".to_string(),
                        progress: 5,
                        target: 5,
                        is_completed: true,
                        is_claimed: false,
                        isk_reward: 500_000,
                        sp_reward: 2500,
                    },
                    DailyOpportunity {
                        id: "daily_industrialist_01".to_string(),
                        name: "Mine Ore".to_string(),
                        description: "Mine 1,000 units of ore from asteroid belts".to_string(),
                        progress: 1000,
                        target: 1000,
                        is_completed: true,
                        is_claimed: false,
                        isk_reward: 500_000,
                        sp_reward: 2500,
                    },
                    DailyOpportunity {
                        id: "daily_explorer_01".to_string(),
                        name: "Scan Cosmic Signatures".to_string(),
                        description: "Fully scan down 2 cosmic signatures using core scanner probes".to_string(),
                        progress: 1,
                        target: 2,
                        is_completed: false,
                        is_claimed: false,
                        isk_reward: 500_000,
                        sp_reward: 2500,
                    },
                    DailyOpportunity {
                        id: "daily_soldier_01".to_string(),
                        name: "Manufacture Item".to_string(),
                        description: "Produce 1 item in an industrial facility".to_string(),
                        progress: 0,
                        target: 1,
                        is_completed: false,
                        is_claimed: false,
                        isk_reward: 500_000,
                        sp_reward: 2500,
                    },
                ];

                let resp = GetDailyOpportunitiesResponse {
                    character_id: cid,
                    opportunities,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 3. eve_public.character.v1.ClaimDailyOpportunityRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            ClaimDailyOpportunityRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req =
                    ClaimDailyOpportunityRequest::decode_proto(&any.value).unwrap_or_default();
                let cid = ctx
                    .effective_character_id()
                    .or(req.character_id)
                    .unwrap_or(90_000_001);

                let opp_id = if req.opportunity_id.is_empty() {
                    "daily_enforcer_01".to_string()
                } else {
                    req.opportunity_id
                };

                let isk_reward = 500_000i64;
                let sp_reward = 2500u32;

                // Credit ISK and SP to character
                db_c.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
                        ch.balance.0 += isk_reward * 100;
                        ch.free_skill_points += sp_reward as f64;
                    }
                });

                let resp = ClaimDailyOpportunityResponse {
                    success: true,
                    opportunity_id: opp_id,
                    isk_reward,
                    sp_reward,
                    message: "Daily opportunity reward claimed successfully".to_string(),
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 4. eve_public.character.v1.GetAchievementsRequest
    {
        router.register_handler(
            GetAchievementsRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetAchievementsRequest::decode_proto(&any.value).unwrap_or_default();
                let cid = ctx
                    .effective_character_id()
                    .or(req.character_id)
                    .unwrap_or(90_000_001);

                let achievements = vec![
                    Achievement {
                        id: "ach_first_warp".to_string(),
                        name: "Into the Unknown".to_string(),
                        category: "Exploration".to_string(),
                        current_progress: 1,
                        target_progress: 1,
                        is_unlocked: true,
                        unlocked_at_unix: 1_760_000_000,
                    },
                    Achievement {
                        id: "ach_first_market".to_string(),
                        name: "Open for Business".to_string(),
                        category: "Economy".to_string(),
                        current_progress: 1,
                        target_progress: 1,
                        is_unlocked: true,
                        unlocked_at_unix: 1_760_000_000,
                    },
                    Achievement {
                        id: "ach_skills_10".to_string(),
                        name: "Eager Learner".to_string(),
                        category: "Progression".to_string(),
                        current_progress: 10,
                        target_progress: 10,
                        is_unlocked: true,
                        unlocked_at_unix: 1_760_000_000,
                    },
                    Achievement {
                        id: "ach_destroy_pirates_50".to_string(),
                        name: "Law Enforcement".to_string(),
                        category: "Combat".to_string(),
                        current_progress: 15,
                        target_progress: 50,
                        is_unlocked: false,
                        unlocked_at_unix: 0,
                    },
                ];

                let resp = GetAchievementsResponse {
                    character_id: cid,
                    achievements,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 5. eve_public.character.v1.ClaimAchievementRequest
    {
        router.register_handler(
            ClaimAchievementRequest::FULL_NAME,
            Arc::new(move |_ctx, any, _dyn_msg| {
                let req = ClaimAchievementRequest::decode_proto(&any.value).unwrap_or_default();
                let resp = ClaimAchievementResponse {
                    success: true,
                    achievement_id: req.achievement_id,
                    message: "Achievement reward claimed successfully".to_string(),
                };
                Ok(resp.to_any())
            }),
        );
    }

    // Fallback prefix wildcard handler for character v1 envelopes
    router.register_handler(
        "eve_public.character.v1.*",
        Arc::new(|ctx, _any, _dyn_msg| {
            let cid = ctx.effective_character_id().unwrap_or(90_000_001);
            let resp = GetProgressionResponse {
                character_id: cid,
                total_sp: 1_000_000,
                unallocated_sp: 25_000,
                skills_count: 35,
                career_points: 50,
                attributes: Some(CharacterAttributes {
                    intelligence: 20.0,
                    memory: 20.0,
                    charisma: 19.0,
                    perception: 20.0,
                    willpower: 20.0,
                }),
            };
            Ok(resp.to_any())
        }),
    );
}

// ============================================================================
// Q-5: Fitting Simulation Services (`eve_public.fitting.v1.*`)
// ============================================================================

pub fn register_q5_fitting_simulation_services(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    // 1. eve_public.fitting.v1.SimulateFittingRequest
    {
        let db_c = Arc::clone(&db);
        let sde_c = Arc::clone(&sde);
        router.register_handler(
            SimulateFittingRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = SimulateFittingRequest::decode_proto(&any.value).map_err(|e| {
                    (
                        STATUS_BAD_REQUEST,
                        format!("Failed to decode SimulateFittingRequest: {e}"),
                    )
                })?;

                let ship_type_id = req.ship_type_id;
                if !sde_c.types.contains_key(&(ship_type_id as TypeId)) {
                    let resp = SimulateFittingResponse {
                        ship_type_id,
                        is_valid: false,
                        validation_errors: vec![format!("Unknown ship type ID {ship_type_id}")],
                        stats: None,
                    };
                    return Ok(resp.to_any());
                }

                let mut engine =
                    ShipFittingEngine::from_sde(1, ship_type_id as TypeId, &sde_c);

                // Set character skills
                let cid_opt = req.character_id.or_else(|| ctx.effective_character_id());
                let skills: HashMap<TypeId, u8> = if let Some(cid) = cid_opt {
                    db_c.with_snapshot(|snap| {
                        snap.skills
                            .get(&(cid as i64))
                            .map(|m| {
                                m.iter()
                                    .map(|(&tid, r)| (tid, r.level as u8))
                                    .collect()
                            })
                            .unwrap_or_default()
                    })
                } else {
                    HashMap::new()
                };

                if !skills.is_empty() {
                    engine.set_character_skills(skills);
                } else if req.character_id.is_none() {
                    // Default to All-5 simulation when no specific character is requested
                    let all_5: HashMap<TypeId, u8> = sde_c
                        .types
                        .values()
                        .filter(|t| t.category_id == 16)
                        .map(|t| (t.type_id, 5))
                        .collect();
                    engine.set_character_skills(all_5);
                }

                let mut validation_errors = Vec::new();
                for (idx, m) in req.modules.iter().enumerate() {
                    let item_id = 100_000 + idx as i64;
                    match engine.fit_and_online_module(
                        item_id,
                        m.type_id as TypeId,
                        m.flag_id as u16,
                        &sde_c,
                    ) {
                        Ok(()) => {
                            if m.state == 2 {
                                if let Some(mod_entry) = engine.modules.get_mut(&item_id) {
                                    mod_entry.state = ModuleState::Active;
                                }
                            } else if m.state == 3 {
                                if let Some(mod_entry) = engine.modules.get_mut(&item_id) {
                                    mod_entry.state = ModuleState::Overloaded;
                                }
                            }
                        }
                        Err(err) => {
                            validation_errors.push(err);
                        }
                    }
                }

                // Evaluate context and compute Dogma attributes
                let eval_ctx = engine.evaluated_context(&sde_c);

                let cpu_used = eval_ctx.ship.get_attribute(attr_ids::CPU_LOAD, &sde_c);
                let cpu_output = eval_ctx.ship.get_attribute(attr_ids::CPU_OUTPUT, &sde_c);
                let powergrid_used = eval_ctx.ship.get_attribute(attr_ids::POWER_LOAD, &sde_c);
                let powergrid_output = eval_ctx.ship.get_attribute(attr_ids::POWER_OUTPUT, &sde_c);
                let calibration_used = eval_ctx.ship.get_attribute(1153, &sde_c); // UPGRADE_COST
                let calibration_output = eval_ctx.ship.get_attribute(1137, &sde_c); // UPGRADE_CAPACITY
                let capacitor_capacity =
                    eval_ctx.ship.get_attribute(attr_ids::CAPACITOR_CAPACITY, &sde_c);
                let capacitor_recharge_rate =
                    eval_ctx.ship.get_attribute(attr_ids::RECHARGE_RATE, &sde_c);
                let max_velocity = eval_ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde_c);
                let mass = eval_ctx.ship.get_attribute(attr_ids::MASS, &sde_c);
                let inertia_modifier = eval_ctx.ship.get_attribute(attr_ids::AGILITY, &sde_c);
                let align_time_seconds = if mass > 0.0 && inertia_modifier > 0.0 {
                    (inertia_modifier * mass / 1_000_000.0) * 2.0_f64.ln()
                } else {
                    0.0
                };
                let shield_hp = eval_ctx.ship.get_attribute(attr_ids::SHIELD_CAPACITY, &sde_c);
                let armor_hp = eval_ctx.ship.get_attribute(attr_ids::ARMOR_HP, &sde_c);
                let hull_hp = eval_ctx.ship.get_attribute(attr_ids::HP_STRUCT, &sde_c);
                let effective_hp = shield_hp + armor_hp + hull_hp;

                // Estimate DPS from fitted active/online weapons
                let mut dps = 0.0f64;
                for item in &eval_ctx.ship_located_items {
                    let em = item.get_attribute(attr_ids::EM_DAMAGE, &sde_c);
                    let ex = item.get_attribute(attr_ids::EXPLOSIVE_DAMAGE, &sde_c);
                    let kin = item.get_attribute(attr_ids::KINETIC_DAMAGE, &sde_c);
                    let th = item.get_attribute(attr_ids::THERMAL_DAMAGE, &sde_c);
                    let dmg = em + ex + kin + th;
                    let dur = item.get_attribute(attr_ids::DURATION, &sde_c);
                    if dmg > 0.0 && dur > 0.0 {
                        dps += dmg / (dur / 1000.0);
                    }
                }

                if cpu_used > cpu_output + 1e-5 {
                    validation_errors.push(format!(
                        "CPU load ({cpu_used:.1}) exceeds ship capacity ({cpu_output:.1})"
                    ));
                }
                if powergrid_used > powergrid_output + 1e-5 {
                    validation_errors.push(format!(
                        "Powergrid load ({powergrid_used:.1}) exceeds ship capacity ({powergrid_output:.1})"
                    ));
                }

                let is_valid = validation_errors.is_empty();
                let stats = FittingStats {
                    cpu_used,
                    cpu_output,
                    powergrid_used,
                    powergrid_output,
                    calibration_used,
                    calibration_output,
                    capacitor_capacity,
                    capacitor_recharge_rate,
                    max_velocity,
                    mass,
                    inertia_modifier,
                    align_time_seconds,
                    shield_hp,
                    armor_hp,
                    hull_hp,
                    effective_hp,
                    dps,
                };

                let resp = SimulateFittingResponse {
                    ship_type_id,
                    is_valid,
                    validation_errors,
                    stats: Some(stats),
                };
                Ok(resp.to_any())
            }),
        );
    }

    // Fallback prefix wildcard handler for fitting v1 envelopes
    router.register_handler(
        "eve_public.fitting.v1.*",
        Arc::new(|_ctx, _any, _dyn_msg| {
            let resp = SimulateFittingResponse {
                ship_type_id: 587,
                is_valid: true,
                validation_errors: Vec::new(),
                stats: Some(FittingStats {
                    cpu_used: 120.0,
                    cpu_output: 140.0,
                    powergrid_used: 35.0,
                    powergrid_output: 45.0,
                    calibration_used: 0.0,
                    calibration_output: 400.0,
                    capacitor_capacity: 250.0,
                    capacitor_recharge_rate: 125.0,
                    max_velocity: 350.0,
                    mass: 1_000_000.0,
                    inertia_modifier: 3.2,
                    align_time_seconds: 2.22,
                    shield_hp: 450.0,
                    armor_hp: 350.0,
                    hull_hp: 400.0,
                    effective_hp: 1200.0,
                    dps: 55.0,
                }),
            };
            Ok(resp.to_any())
        }),
    );
}

// ============================================================================
// Q-6: Market & Wallet Services (`eve_public.market.v1.*`, `eve_public.wallet.v1.*`)
// ============================================================================

pub fn register_q6_market_wallet_services(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    // 1. eve_public.market.v1.GetOrdersRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            GetOrdersRequest::FULL_NAME,
            Arc::new(move |_ctx, any, _dyn_msg| {
                let req = GetOrdersRequest::decode_proto(&any.value).unwrap_or_default();
                let region_id = req.region_id;
                let type_id = req.type_id;
                let order_type = req.order_type.to_lowercase();

                let orders: Vec<MarketOrder> = db_c.with_snapshot(|snap| {
                    snap.market_orders
                        .values()
                        .filter(|o| {
                            (region_id == 0 || o.region_id == region_id)
                                && (type_id == 0 || o.type_id == type_id)
                                && o.vol_remaining > 0
                                && match order_type.as_str() {
                                    "buy" => o.is_bid,
                                    "sell" => !o.is_bid,
                                    _ => true,
                                }
                        })
                        .map(|o| MarketOrder {
                            order_id: o.order_id as u64,
                            type_id: o.type_id,
                            is_buy_order: o.is_bid,
                            price: o.price.0 as f64 / 100.0,
                            volume_entered: o.vol_entered,
                            volume_remain: o.vol_remaining,
                            min_volume: o.min_volume,
                            order_range: 32767,
                            duration_days: 90,
                            location_id: o.station_id,
                            solar_system_id: 30_000_142,
                            issued_unix: 1_760_000_000,
                        })
                        .collect()
                });

                let resp = GetOrdersResponse { orders };
                Ok(resp.to_any())
            }),
        );
    }

    // 2. eve_public.market.v1.GetMarketHistoryRequest
    {
        let db_c = Arc::clone(&db);
        let sde_c = Arc::clone(&sde);
        router.register_handler(
            GetMarketHistoryRequest::FULL_NAME,
            Arc::new(move |_ctx, any, _dyn_msg| {
                let req = GetMarketHistoryRequest::decode_proto(&any.value).unwrap_or_default();
                let region_id = req.region_id;
                let type_id = req.type_id;

                let entries = db_c.with_snapshot(|snap| {
                    let matching: Vec<&_> = snap
                        .market_orders
                        .values()
                        .filter(|o| {
                            (region_id == 0 || o.region_id == region_id)
                                && (type_id == 0 || o.type_id == type_id)
                        })
                        .collect();

                    if !matching.is_empty() {
                        let total_vol: i64 = matching.iter().map(|o| o.vol_entered as i64).sum();
                        let count = matching.len() as i64;
                        let prices: Vec<f64> = matching
                            .iter()
                            .map(|o| o.price.0 as f64 / 100.0)
                            .collect();
                        let min_p = prices.iter().cloned().fold(f64::INFINITY, f64::min);
                        let max_p = prices.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                        let avg_p = prices.iter().sum::<f64>() / prices.len() as f64;

                        vec![MarketHistoryEntry {
                            date_unix: 1_760_000_000,
                            average_price: avg_p,
                            highest_price: max_p,
                            lowest_price: min_p,
                            order_count: count,
                            volume: total_vol,
                        }]
                    } else {
                        // Fallback reasonable market history based on SDE base price
                        let base_price = sde_c
                            .types
                            .get(&type_id)
                            .map(|t| t.base_price.max(10.0))
                            .unwrap_or(100.0);
                        vec![MarketHistoryEntry {
                            date_unix: 1_760_000_000,
                            average_price: base_price,
                            highest_price: base_price * 1.05,
                            lowest_price: base_price * 0.95,
                            order_count: 12,
                            volume: 1500,
                        }]
                    }
                });

                let resp = GetMarketHistoryResponse {
                    region_id,
                    type_id,
                    entries,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 3. eve_public.wallet.v1.GetBalanceRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            GetBalanceRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetBalanceRequest::decode_proto(&any.value).unwrap_or_default();
                let cid = ctx
                    .effective_character_id()
                    .or(req.character_id)
                    .ok_or_else(|| (STATUS_FORBIDDEN, "Active character required".into()))?;

                let (cents, plex) = db_c.with_snapshot(|snap| {
                    let ch_opt = snap.characters.get(&(cid as i64));
                    let balance_cents = ch_opt.map(|c| c.balance.0).unwrap_or(0);
                    let plex_bal = ch_opt.map(|c| c.plex_balance).unwrap_or(500);
                    (balance_cents, plex_bal)
                });

                let resp = GetBalanceResponse {
                    character_id: cid,
                    isk_balance_units: cents / 100,
                    isk_balance_cents: cents,
                    plex_balance: plex,
                    aur_balance: 0,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 4. eve_public.wallet.v1.GetJournalRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            GetJournalRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetJournalRequest::decode_proto(&any.value).unwrap_or_default();
                let cid = ctx
                    .effective_character_id()
                    .or(req.character_id)
                    .ok_or_else(|| (STATUS_FORBIDDEN, "Active character required".into()))?;

                let limit = req.limit.unwrap_or(50) as usize;

                let entries = db_c.with_snapshot(|snap| {
                    snap.wallet_journal
                        .iter()
                        .take(limit)
                        .map(|j| WalletJournalEntry {
                            entry_id: j.entry_id as u64,
                            date_unix: 1_760_000_000,
                            ref_type_id: 1,
                            amount: j.amount.0 / 100,
                            balance: j.balance_after.0 / 100,
                            reason: j.reason.clone(),
                        })
                        .collect()
                });

                let resp = GetJournalResponse {
                    character_id: cid,
                    entries,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // Fallback prefix wildcard handlers for market & wallet v1 envelopes
    router.register_handler(
        "eve_public.market.v1.*",
        Arc::new(|_ctx, _any, _dyn_msg| {
            let resp = GetOrdersResponse { orders: Vec::new() };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.wallet.v1.*",
        Arc::new(|ctx, _any, _dyn_msg| {
            let cid = ctx.effective_character_id().unwrap_or(90_000_001);
            let resp = GetBalanceResponse {
                character_id: cid,
                isk_balance_units: 10_000_000,
                isk_balance_cents: 1_000_000_000,
                plex_balance: 500,
                aur_balance: 0,
            };
            Ok(resp.to_any())
        }),
    );
}

// ============================================================================
// Q-7: Space & Combat Services (`eve_public.solarsystem.v1.*`, `eve_public.structure.v1.*`, etc.)
// ============================================================================

pub fn register_q7_space_and_combat_services(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    // 1. eve_public.solarsystem.v1.GetSolarSystemRequest
    {
        let sde_c = Arc::clone(&sde);
        router.register_handler(
            GetSolarSystemRequest::FULL_NAME,
            Arc::new(move |_ctx, any, _dyn_msg| {
                let req = GetSolarSystemRequest::decode_proto(&any.value).map_err(|e| {
                    (
                        STATUS_BAD_REQUEST,
                        format!("Failed to decode GetSolarSystemRequest: {e}"),
                    )
                })?;
                let sid = req.solar_system_id;
                let sys = sde_c.solar_systems.get(&sid).ok_or_else(|| {
                    (STATUS_NOT_FOUND, format!("Solar system {sid} not found"))
                })?;

                let mut stargate_ids: Vec<u64> = sde_c
                    .jumps
                    .values()
                    .filter(|g| g.from_solar_system_id == sid)
                    .map(|g| g.stargate_id as u64)
                    .collect();
                stargate_ids.sort_unstable();

                let mut station_ids: Vec<u64> = sys
                    .stations
                    .iter()
                    .map(|s| s.station_id as u64)
                    .collect();
                station_ids.sort_unstable();

                let resp = GetSolarSystemResponse {
                    solar_system_id: sid,
                    solar_system_name: sys.name.clone(),
                    constellation_id: sys.constellation_id,
                    region_id: sys.region_id,
                    security_status: sys.security_status,
                    stargate_ids,
                    station_ids,
                    star_id: None,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 2. eve_public.structure.v1.GetStructureRequest
    {
        let db_c = Arc::clone(&db);
        let sde_c = Arc::clone(&sde);
        router.register_handler(
            GetStructureRequest::FULL_NAME,
            Arc::new(move |_ctx, any, _dyn_msg| {
                let req = GetStructureRequest::decode_proto(&any.value).map_err(|e| {
                    (
                        STATUS_BAD_REQUEST,
                        format!("Failed to decode GetStructureRequest: {e}"),
                    )
                })?;
                let sid = req.structure_id;

                let struct_opt = db_c.with_snapshot(|snap| snap.structures.get(&(sid as i64)).cloned());
                if let Some(st) = struct_opt {
                    let resp = GetStructureResponse {
                        structure_id: sid,
                        name: st.name,
                        type_id: st.type_id,
                        solar_system_id: st.solar_system_id as u32,
                        owner_corporation_id: st.owner_corp_id as u64,
                        state: st.state,
                        fuel_expires_at_unix: if st.fuel_blocks > 0 { 1_760_000_000 } else { 0 },
                        position_x: st.position[0],
                        position_y: st.position[1],
                        position_z: st.position[2],
                    };
                    return Ok(resp.to_any());
                }

                // Check SDE stations
                let sta_opt = sde_c
                    .solar_systems
                    .iter()
                    .find_map(|(&sys_id, sys)| {
                        sys.stations.iter().find(|s| s.station_id == sid as i64).map(|s| (sys_id, s))
                    });
                if let Some((sys_id, sta)) = sta_opt {
                    let resp = GetStructureResponse {
                        structure_id: sid,
                        name: sta.name.clone(),
                        type_id: sta.station_type_id,
                        solar_system_id: sys_id,
                        owner_corporation_id: sta.owner_corporation_id as u64,
                        state: "Online".to_string(),
                        fuel_expires_at_unix: 0,
                        position_x: sta.position.x,
                        position_y: sta.position.y,
                        position_z: sta.position.z,
                    };
                    return Ok(resp.to_any());
                }

                Err((STATUS_NOT_FOUND, format!("Structure {sid} not found")))
            }),
        );
    }

    // 3. eve_public.killmail.v1.GetKillmailRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            GetKillmailRequest::FULL_NAME,
            Arc::new(move |_ctx, any, _dyn_msg| {
                let req = GetKillmailRequest::decode_proto(&any.value).map_err(|e| {
                    (
                        STATUS_BAD_REQUEST,
                        format!("Failed to decode GetKillmailRequest: {e}"),
                    )
                })?;
                let kid = req.killmail_id;

                let km_opt = db_c.with_snapshot(|snap| snap.killmails.get(&(kid as i64)).cloned());
                let Some(km) = km_opt else {
                    return Err((STATUS_NOT_FOUND, format!("Killmail {kid} not found")));
                };

                let hash = crate::esi::sample_routes::compute_killmail_hash(
                    km.kill_id,
                    km.victim_character_id,
                    km.attacker_character_id,
                    km.ship_type_id as u32,
                );

                if let Some(ref h) = req.killmail_hash {
                    if !h.is_empty() && !h.eq_ignore_ascii_case(&hash) {
                        return Err((STATUS_FORBIDDEN, format!("Invalid killmail hash for killmail {kid}")));
                    }
                }

                let resp = GetKillmailResponse {
                    killmail_id: km.kill_id as u64,
                    killmail_time_unix: 1_700_000_000,
                    solar_system_id: km.solar_system_id as u32,
                    ship_type_id: km.ship_type_id as u32,
                    victim_character_id: km.victim_character_id as u64,
                    attacker_character_id: km.attacker_character_id as u64,
                    total_value_isk: km.total_value_isk,
                    killmail_hash: hash,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 4. eve_public.ship.v1.GetShipRequest
    {
        let db_c = Arc::clone(&db);
        let sde_c = Arc::clone(&sde);
        router.register_handler(
            GetShipRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetShipRequest::decode_proto(&any.value).unwrap_or_default();
                let cid = ctx.effective_character_id().or(req.character_id).unwrap_or(90_000_001);

                let (ship_id, ship_type_id) = db_c.with_snapshot(|snap| {
                    let c_opt = snap.characters.get(&(cid as i64));
                    let sid = req.ship_id.map(|s| s as i64).or_else(|| c_opt.map(|c| c.active_ship_id)).unwrap_or(1001);
                    let stid = snap.items.get(&sid).map(|i| i.type_id).unwrap_or(587);
                    (sid as u64, stid)
                });

                let ship_name = sde_c
                    .types
                    .get(&ship_type_id)
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| "Unknown Ship".to_string());

                let fit = ShipFittingEngine::from_sde(ship_id as i64, ship_type_id, &sde_c);
                let eval = fit.evaluated_context(&sde_c);

                let max_velocity = eval.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde_c);
                let shield_hp = eval.ship.get_attribute(attr_ids::SHIELD_CAPACITY, &sde_c);
                let armor_hp = eval.ship.get_attribute(attr_ids::ARMOR_HP, &sde_c);
                let hull_hp = eval.ship.get_attribute(attr_ids::HP_STRUCT, &sde_c);

                let resp = GetShipResponse {
                    ship_id,
                    ship_type_id,
                    ship_name,
                    max_velocity,
                    shield_hp,
                    armor_hp,
                    hull_hp,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 5. eve_public.stargate.v1.GetStargateRequest
    {
        let sde_c = Arc::clone(&sde);
        router.register_handler(
            GetStargateRequest::FULL_NAME,
            Arc::new(move |_ctx, any, _dyn_msg| {
                let req = GetStargateRequest::decode_proto(&any.value).map_err(|e| {
                    (
                        STATUS_BAD_REQUEST,
                        format!("Failed to decode GetStargateRequest: {e}"),
                    )
                })?;
                let gid = req.stargate_id as i64;
                let gate = sde_c.jumps.get(&gid).ok_or_else(|| {
                    (STATUS_NOT_FOUND, format!("Stargate {gid} not found"))
                })?;

                let resp = GetStargateResponse {
                    stargate_id: req.stargate_id,
                    name: format!("Stargate to {}", gate.to_solar_system_id),
                    solar_system_id: gate.from_solar_system_id,
                    destination_stargate_id: gate.destination_stargate_id as u64,
                    destination_system_id: gate.to_solar_system_id,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 6. eve_public.station.v1.GetStationRequest
    {
        let sde_c = Arc::clone(&sde);
        router.register_handler(
            GetStationRequest::FULL_NAME,
            Arc::new(move |_ctx, any, _dyn_msg| {
                let req = GetStationRequest::decode_proto(&any.value).map_err(|e| {
                    (
                        STATUS_BAD_REQUEST,
                        format!("Failed to decode GetStationRequest: {e}"),
                    )
                })?;
                let sta_id = req.station_id as i64;
                let sta_opt = sde_c
                    .solar_systems
                    .iter()
                    .find_map(|(&sys_id, sys)| {
                        sys.stations.iter().find(|s| s.station_id == sta_id).map(|s| (sys_id, s))
                    });
                let Some((sys_id, sta)) = sta_opt else {
                    return Err((STATUS_NOT_FOUND, format!("Station {sta_id} not found")));
                };

                let resp = GetStationResponse {
                    station_id: req.station_id,
                    name: sta.name.clone(),
                    solar_system_id: sys_id,
                    type_id: sta.station_type_id,
                    owner_corporation_id: sta.owner_corporation_id as u64,
                    reprocessing_efficiency: 0.5,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 7. eve_public.probe.v1.GetProbeScansRequest
    {
        router.register_handler(
            GetProbeScansRequest::FULL_NAME,
            Arc::new(move |_ctx, any, _dyn_msg| {
                let req = GetProbeScansRequest::decode_proto(&any.value).map_err(|e| {
                    (
                        STATUS_BAD_REQUEST,
                        format!("Failed to decode GetProbeScansRequest: {e}"),
                    )
                })?;
                let sid = req.solar_system_id;
                let resp = GetProbeScansResponse {
                    solar_system_id: sid,
                    signatures: vec![
                        ProbeScanSignature {
                            signature_id: format!("SIG-{:03}", sid % 1000),
                            signature_type: "Cosmic Anomaly".to_string(),
                            signal_strength: 1.0,
                        },
                        ProbeScanSignature {
                            signature_id: format!("REL-{:03}", (sid + 13) % 1000),
                            signature_type: "Relic Site".to_string(),
                            signal_strength: 0.72,
                        },
                    ],
                };
                Ok(resp.to_any())
            }),
        );
    }

    // Prefix wildcard handlers for space & combat envelopes
    router.register_handler(
        "eve_public.solarsystem.v1.*",
        Arc::new(|_ctx, _any, _dyn_msg| {
            let resp = GetSolarSystemResponse {
                solar_system_id: 30000142,
                solar_system_name: "Jita".to_string(),
                constellation_id: 20000020,
                region_id: 10000002,
                security_status: 0.945,
                stargate_ids: vec![50000001, 50000002],
                station_ids: vec![60003760],
                star_id: Some(3799),
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.structure.v1.*",
        Arc::new(|_ctx, _any, _dyn_msg| {
            let resp = GetStructureResponse {
                structure_id: 1_000_000_001,
                name: "Astrahus Citadel".to_string(),
                type_id: 35832,
                solar_system_id: 30000142,
                owner_corporation_id: 1000001,
                state: "Online".to_string(),
                fuel_expires_at_unix: 1_760_000_000,
                position_x: 0.0,
                position_y: 0.0,
                position_z: 0.0,
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.killmail.v1.*",
        Arc::new(|_ctx, _any, _dyn_msg| {
            let resp = GetKillmailResponse {
                killmail_id: 9001,
                killmail_time_unix: 1_700_000_000,
                solar_system_id: 30000142,
                ship_type_id: 587,
                victim_character_id: 90000002,
                attacker_character_id: 90000001,
                total_value_isk: 2_500_000.0,
                killmail_hash: "default_killmail_hash".to_string(),
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.ship.v1.*",
        Arc::new(|_ctx, _any, _dyn_msg| {
            let resp = GetShipResponse {
                ship_id: 1001,
                ship_type_id: 587,
                ship_name: "Rifter".to_string(),
                max_velocity: 350.0,
                shield_hp: 450.0,
                armor_hp: 400.0,
                hull_hp: 350.0,
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.stargate.v1.*",
        Arc::new(|_ctx, _any, _dyn_msg| {
            let resp = GetStargateResponse {
                stargate_id: 50000001,
                name: "Stargate (Jita)".to_string(),
                solar_system_id: 30000142,
                destination_stargate_id: 50000002,
                destination_system_id: 30000144,
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.station.v1.*",
        Arc::new(|_ctx, _any, _dyn_msg| {
            let resp = GetStationResponse {
                station_id: 60003760,
                name: "Jita IV - Moon 4 - Caldari Navy Assembly Plant".to_string(),
                solar_system_id: 30000142,
                type_id: 52678,
                owner_corporation_id: 1000002,
                reprocessing_efficiency: 0.5,
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.probe.v1.*",
        Arc::new(|_ctx, _any, _dyn_msg| {
            let resp = GetProbeScansResponse {
                solar_system_id: 30000142,
                signatures: Vec::new(),
            };
            Ok(resp.to_any())
        }),
    );
}

// ============================================================================
// Q-8: Sovereignty, Faction Warfare & Events (`eve_public.sovereignty.v1.*`, etc.)
// ============================================================================

pub fn register_q8_sovereignty_and_events_services(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    _sde: Arc<SdeRegistry>,
) {
    // 1. eve_public.sovereignty.v1.GetSovereigntyRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            GetSovereigntyRequest::FULL_NAME,
            Arc::new(move |_ctx, any, _dyn_msg| {
                let req = GetSovereigntyRequest::decode_proto(&any.value).map_err(|e| {
                    (
                        STATUS_BAD_REQUEST,
                        format!("Failed to decode GetSovereigntyRequest: {e}"),
                    )
                })?;
                let sid = req.solar_system_id;

                let (alliance_id, mil, ind, strat, dbs) = db_c.with_snapshot(|snap| {
                    if let Some(st) = snap.system_states.get(&sid) {
                        (
                            st.sov_alliance_id.map(|a| a as u64),
                            st.military_index as u32,
                            st.industrial_index as u32,
                            st.strategic_index as u32,
                            st.dbs_multiplier,
                        )
                    } else {
                        (None, 0, 0, 0, 1.0)
                    }
                });

                let resp = GetSovereigntyResponse {
                    solar_system_id: sid,
                    alliance_id,
                    corporation_id: None,
                    faction_id: None,
                    military_index: mil,
                    industrial_index: ind,
                    strategic_index: strat,
                    dbs_multiplier: dbs,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 2. eve_public.factionwarfare.v1.GetFactionWarfareRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            GetFactionWarfareRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetFactionWarfareRequest::decode_proto(&any.value).unwrap_or_default();
                let cid = ctx.effective_character_id().or(req.character_id);

                let (fid, contested) = db_c.with_snapshot(|snap| {
                    let f = cid
                        .and_then(|c| snap.characters.get(&(c as i64)))
                        .and_then(|c| c.war_faction_id)
                        .or(req.faction_id.map(|x| x as i64))
                        .unwrap_or(500_001);
                    let ct = snap
                        .system_states
                        .values()
                        .filter(|s| s.fw_contested_pct > 0.0)
                        .count() as u32;
                    (f as u64, ct)
                });

                let resp = GetFactionWarfareResponse {
                    faction_id: fid,
                    current_rank: 3,
                    highest_rank: 4,
                    kills: 35,
                    victory_points: 1500,
                    contested_systems: contested,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 3. eve_public.projectdiscovery.v1.GetProjectDiscoveryRequest
    {
        router.register_handler(
            GetProjectDiscoveryRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetProjectDiscoveryRequest::decode_proto(&any.value).unwrap_or_default();
                let cid = ctx.effective_character_id().or(req.character_id).unwrap_or(90_000_001);

                let resp = GetProjectDiscoveryResponse {
                    character_id: cid,
                    score_percent: 94.5,
                    rank: 16,
                    daily_xp_collected: 800,
                    daily_xp_cap: 2000,
                    active_task_id: "pd_flow_cytometry_task_42".to_string(),
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 4. eve_public.operation.v1.GetOperationsRequest
    {
        router.register_handler(
            GetOperationsRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetOperationsRequest::decode_proto(&any.value).unwrap_or_default();
                let cid = ctx.effective_character_id().or(req.character_id).unwrap_or(90_000_001);

                let resp = GetOperationsResponse {
                    character_id: cid,
                    operations: vec![
                        OperationItem {
                            operation_id: 101,
                            title: "AIR Career Explorer Phase 1".to_string(),
                            is_completed: true,
                            progress: 1.0,
                        },
                        OperationItem {
                            operation_id: 102,
                            title: "AIR Career Enforcer Phase 1".to_string(),
                            is_completed: false,
                            progress: 0.65,
                        },
                    ],
                };
                Ok(resp.to_any())
            }),
        );
    }

    // Fallback wildcards for Q-8
    router.register_handler(
        "eve_public.sovereignty.v1.*",
        Arc::new(|_ctx, _any, _dyn_msg| {
            let resp = GetSovereigntyResponse {
                solar_system_id: 30000142,
                alliance_id: None,
                corporation_id: None,
                faction_id: None,
                military_index: 0,
                industrial_index: 0,
                strategic_index: 0,
                dbs_multiplier: 1.0,
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.factionwarfare.v1.*",
        Arc::new(|_ctx, _any, _dyn_msg| {
            let resp = GetFactionWarfareResponse {
                faction_id: 500001,
                current_rank: 1,
                highest_rank: 1,
                kills: 0,
                victory_points: 0,
                contested_systems: 0,
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.projectdiscovery.v1.*",
        Arc::new(|ctx, _any, _dyn_msg| {
            let cid = ctx.effective_character_id().unwrap_or(90_000_001);
            let resp = GetProjectDiscoveryResponse {
                character_id: cid,
                score_percent: 90.0,
                rank: 1,
                daily_xp_collected: 0,
                daily_xp_cap: 2000,
                active_task_id: "default_pd_task".to_string(),
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.operation.v1.*",
        Arc::new(|ctx, _any, _dyn_msg| {
            let cid = ctx.effective_character_id().unwrap_or(90_000_001);
            let resp = GetOperationsResponse {
                character_id: cid,
                operations: Vec::new(),
            };
            Ok(resp.to_any())
        }),
    );
}

// ============================================================================
// Q-9: Cosmetics & App Infrastructure (`eve_public.cosmetic.v1.*`, `eve_public.settings.v1.*`, etc.)
// ============================================================================

pub fn register_q9_cosmetics_and_client_services(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    _sde: Arc<SdeRegistry>,
) {
    // 1. eve_public.cosmetic.v1.GetCosmeticsRequest
    {
        let db_c = Arc::clone(&db);
        router.register_handler(
            GetCosmeticsRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetCosmeticsRequest::decode_proto(&any.value).unwrap_or_default();
                let cid = ctx.effective_character_id().or(req.character_id).unwrap_or(90_000_001);

                let (licensed, applied) = db_c.with_snapshot(|snap| {
                    let l = snap.licensed_skins.get(&(cid as i64)).cloned().unwrap_or_default();
                    let active_ship = snap.characters.get(&(cid as i64)).map(|c| c.active_ship_id).unwrap_or(0);
                    let a = snap.applied_ship_skins.get(&active_ship).copied();
                    (l, a)
                });

                let resp = GetCosmeticsResponse {
                    character_id: cid,
                    licensed_skin_ids: licensed,
                    applied_skin_id: applied,
                    evermarks_balance: 1400,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 2. eve_public.settings.v1.GetClientSettingsRequest
    {
        router.register_handler(
            GetClientSettingsRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetClientSettingsRequest::decode_proto(&any.value).unwrap_or_default();
                let uid = ctx.effective_user_id().map(|u| u as u64).or(req.user_id).unwrap_or(1);
                let cid = ctx.effective_character_id().or(req.character_id).unwrap_or(90_000_001);

                let resp = GetClientSettingsResponse {
                    user_id: uid,
                    character_id: cid,
                    settings: vec![
                        ClientSettingEntry {
                            key: "ui.scale".to_string(),
                            value: "100".to_string(),
                        },
                        ClientSettingEntry {
                            key: "audio.master".to_string(),
                            value: "0.8".to_string(),
                        },
                        ClientSettingEntry {
                            key: "graphics.lod".to_string(),
                            value: "high".to_string(),
                        },
                    ],
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 3. eve_public.settings.v1.SetClientSettingsRequest
    {
        router.register_handler(
            SetClientSettingsRequest::FULL_NAME,
            Arc::new(move |_ctx, any, _dyn_msg| {
                let req = SetClientSettingsRequest::decode_proto(&any.value).map_err(|e| {
                    (
                        STATUS_BAD_REQUEST,
                        format!("Failed to decode SetClientSettingsRequest: {e}"),
                    )
                })?;
                let resp = SetClientSettingsResponse {
                    success: true,
                    key: req.key,
                    updated_at_unix: 1_700_000_000,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 4. eve_public.paragon.v1.GetParagonMissionsRequest
    {
        router.register_handler(
            GetParagonMissionsRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetParagonMissionsRequest::decode_proto(&any.value).unwrap_or_default();
                let cid = ctx.effective_character_id().or(req.character_id).unwrap_or(90_000_001);

                let resp = GetParagonMissionsResponse {
                    character_id: cid,
                    evermarks_balance: 1400,
                    missions_completed: 4,
                };
                Ok(resp.to_any())
            }),
        );
    }

    // 5. eve_public.ui.v1.GetUiLayoutRequest
    {
        router.register_handler(
            GetUiLayoutRequest::FULL_NAME,
            Arc::new(move |ctx, any, _dyn_msg| {
                let req = GetUiLayoutRequest::decode_proto(&any.value).unwrap_or_default();
                let cid = ctx.effective_character_id().or(req.character_id).unwrap_or(90_000_001);

                let resp = GetUiLayoutResponse {
                    character_id: cid,
                    layout_json: r#"{"windows":{"chat":{"x":10,"y":500,"w":400,"h":300},"overview":{"x":1500,"y":50,"w":400,"h":600}}}"#.to_string(),
                };
                Ok(resp.to_any())
            }),
        );
    }

    // Fallback wildcards for Q-9
    router.register_handler(
        "eve_public.cosmetic.v1.*",
        Arc::new(|ctx, _any, _dyn_msg| {
            let cid = ctx.effective_character_id().unwrap_or(90_000_001);
            let resp = GetCosmeticsResponse {
                character_id: cid,
                licensed_skin_ids: Vec::new(),
                applied_skin_id: None,
                evermarks_balance: 0,
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.skin.v1.*",
        Arc::new(|ctx, _any, _dyn_msg| {
            let cid = ctx.effective_character_id().unwrap_or(90_000_001);
            let resp = GetCosmeticsResponse {
                character_id: cid,
                licensed_skin_ids: Vec::new(),
                applied_skin_id: None,
                evermarks_balance: 0,
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.settings.v1.*",
        Arc::new(|ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().map(|u| u as u64).unwrap_or(1);
            let cid = ctx.effective_character_id().unwrap_or(90_000_001);
            let resp = GetClientSettingsResponse {
                user_id: uid,
                character_id: cid,
                settings: Vec::new(),
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.client.v1.*",
        Arc::new(|ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().map(|u| u as u64).unwrap_or(1);
            let cid = ctx.effective_character_id().unwrap_or(90_000_001);
            let resp = GetClientSettingsResponse {
                user_id: uid,
                character_id: cid,
                settings: Vec::new(),
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.paragon.v1.*",
        Arc::new(|ctx, _any, _dyn_msg| {
            let cid = ctx.effective_character_id().unwrap_or(90_000_001);
            let resp = GetParagonMissionsResponse {
                character_id: cid,
                evermarks_balance: 0,
                missions_completed: 0,
            };
            Ok(resp.to_any())
        }),
    );

    router.register_handler(
        "eve_public.ui.v1.*",
        Arc::new(|ctx, _any, _dyn_msg| {
            let cid = ctx.effective_character_id().unwrap_or(90_000_001);
            let resp = GetUiLayoutResponse {
                character_id: cid,
                layout_json: "{}".to_string(),
            };
            Ok(resp.to_any())
        }),
    );
}
