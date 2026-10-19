//! `quasar_services` — Quasar Protobuf Backend Services (`Q-3` Identity & Entitlements,
//! `Q-4` Character Progression, `Q-5` Fitting Simulation, `Q-6` Market & Wallet).

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_UNAUTHORIZED,
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
    register_q6_market_wallet_services(router, db, sde);
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
