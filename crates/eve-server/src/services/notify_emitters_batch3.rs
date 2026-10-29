//! `notify_emitters_batch3` — Server-side V24 notification emitters for Batch 3
//! (Social, GM/Cluster, Agents, Industry, Seasons/Campaigns, Cosmetics/FastCheckout, Skills, ESI).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Emits `OnModuleAttributeChange` (8-arg V24 `StateManager.OnModuleAttributeChange`)
/// and, when the item resides inside a ship/container (`loc_id > 0 && loc_id != owner_id`),
/// `OnLocationAttributeChange` (7-arg V24 `StateManager.OnLocationAttributeChange`).
#[allow(clippy::too_many_arguments)]
pub fn emit_setattr_notifications(
    ctx: &ServerContext,
    owner_id: i64,
    loc_id: i64,
    item_id: i64,
    attr_id: u16,
    now_ft: i64,
    val: f64,
    old_val: f64,
) {
    ctx.notify(
        NotificationTarget::Character(owner_id),
        "OnModuleAttributeChange",
        "charid",
        vec![
            PyRep::Integer(owner_id),
            PyRep::Integer(item_id),
            PyRep::Integer(i64::from(attr_id)),
            PyRep::Integer(now_ft),
            PyRep::Real(val),
            PyRep::Real(old_val),
            PyRep::Integer(now_ft),
            PyRep::Bool(true),
        ],
    );
    if loc_id > 0 && loc_id != owner_id {
        ctx.notify(
            NotificationTarget::Character(owner_id),
            "OnLocationAttributeChange",
            "charid",
            vec![
                PyRep::Integer(owner_id),
                PyRep::Integer(loc_id),
                PyRep::Integer(item_id),
                PyRep::Integer(i64::from(attr_id)),
                PyRep::Integer(now_ft),
                PyRep::Real(val),
                PyRep::Real(old_val),
            ],
        );
    }
}

/// Broadcasts `OnClusterShutdownCancelled(explanationLabel)` to all online clients.
pub fn emit_cluster_shutdown_cancelled(ctx: &ServerContext, explanation_label: &str) {
    let label = if explanation_label.trim().is_empty() {
        "UI/Shared/ClusterShutdownDelayed"
    } else {
        explanation_label.trim()
    };
    ctx.notify(
        NotificationTarget::All,
        "OnClusterShutdownCancelled",
        "clientID",
        vec![PyRep::String(label.to_string())],
    );
}

/// Broadcasts `OnGlobalConfigUpdated(key, value)` to all online clients.
pub fn emit_global_config_updated(ctx: &ServerContext, key: &str, val: &PyRep) {
    ctx.notify(
        NotificationTarget::All,
        "OnGlobalConfigUpdated",
        "clientID",
        vec![PyRep::String(key.to_string()), val.clone()],
    );
}

/// Emits `OnAirNpeStateChanged(character_id, new_state_id)` (`AirNpeState.SKIPPED = 3`).
pub fn emit_air_npe_skipped(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnAirNpeStateChanged",
        "charid",
        vec![PyRep::Integer(char_id), PyRep::Integer(3)],
    );
}

/// Emits `OnDatacoreBought(characterID, agentID, balance)` upon R&D agent datacore purchase.
pub fn emit_datacore_bought(ctx: &ServerContext, char_id: i64, agent_id: i64, balance_isk: f64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDatacoreBought",
        "charid",
        vec![
            PyRep::Integer(char_id),
            PyRep::Integer(agent_id),
            PyRep::Real(balance_isk),
        ],
    );
}

/// Emits `OnIncomingAgentMessage`, `OnIncomingTransmission`, and (for Epic Arc agents, `agent_type_id == 10`)
/// `OnEpicJournalChange` upon mission acceptance or completion.
pub fn emit_agent_mission_dialogue(
    ctx: &ServerContext,
    char_id: i64,
    agent_id: i64,
    agent_type_id: u32,
    title: &str,
    body: &str,
    completed: bool,
) {
    let label = if completed {
        "UI/Agents/Dialogue/StandardMissionCompleted"
    } else {
        "UI/Agents/Dialogue/StandardMissionAccepted"
    };
    let clean_header = title.trim_start_matches("[d]").trim();
    let header_str = if clean_header.is_empty() {
        "Agent Transmission"
    } else {
        clean_header
    };
    let text_str = if body.trim().is_empty() {
        header_str
    } else {
        body.trim()
    };
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnIncomingAgentMessage",
        "charid",
        vec![
            PyRep::Integer(agent_id),
            PyRep::Tuple(vec![
                PyRep::String(label.to_string()),
                PyRep::Dict(vec![
                    (
                        PyRep::String("missionName".into()),
                        PyRep::String(header_str.to_string()),
                    ),
                    (PyRep::String("agentID".into()), PyRep::Integer(agent_id)),
                ]),
            ]),
        ],
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnIncomingTransmission",
        "charid",
        vec![
            PyRep::keyval([
                ("header", PyRep::String(header_str.to_string())),
                ("text", PyRep::String(text_str.to_string())),
                (
                    "icon",
                    PyRep::String("res:/UI/Texture/WindowIcons/agent.png".into()),
                ),
                ("agentID", PyRep::Integer(agent_id)),
            ]),
            PyRep::Bool(true),
        ],
    );
    if agent_type_id == 10 {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnEpicJournalChange",
            "charid",
            vec![],
        );
    }
}

/// Emits `OnAllowWarUpdated(corpID, allowWar)` (`id_type = "corpid"`) to each member corporation
/// of `alliance_id` when `allianceRegistry.UpdateAlliance` updates war-eligibility settings.
pub fn emit_allow_war_updated_for_alliance(
    ctx: &ServerContext,
    alliance_id: i64,
    allow_war: bool,
) {
    let member_corps: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let mut corps: Vec<i64> = snap
            .corporations
            .values()
            .filter(|c| c.alliance_id == Some(alliance_id))
            .map(|c| c.corporation_id)
            .collect();
        corps.sort_unstable();
        corps.dedup();
        corps
    });
    ctx.db.with_mut_snapshot(|snap| {
        let mut map: HashMap<i64, bool> = snap
            .service_state_blobs
            .get("corp_allow_war")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        for &cid in &member_corps {
            map.insert(cid, allow_war);
        }
        if let Ok(json) = serde_json::to_string(&map) {
            snap.service_state_blobs
                .insert("corp_allow_war".into(), json);
        }
    });
    for corp_id in member_corps {
        ctx.notify(
            NotificationTarget::Corporation(corp_id),
            "OnAllowWarUpdated",
            "corpid",
            vec![PyRep::Integer(corp_id), PyRep::Bool(allow_war)],
        );
    }
}

/// Emits `OnInvalidateCacheForLocations(locationIDs)` when `location_ids` is non-empty.
pub fn emit_invalidate_cache_for_locations(
    ctx: &ServerContext,
    target: NotificationTarget,
    location_ids: &[i64],
) {
    if location_ids.is_empty() {
        return;
    }
    let list = location_ids.iter().copied().map(PyRep::Integer).collect();
    ctx.notify(
        target,
        "OnInvalidateCacheForLocations",
        "charid",
        vec![PyRep::List(list)],
    );
}

/// Emits `OnCampaignEnrollmentChanged()` when a pilot enrolls in or starts a login campaign.
pub fn emit_campaign_enrollment_changed(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCampaignEnrollmentChanged",
        "charid",
        vec![],
    );
}

/// Emits `OnChallengeExpired(challenge_id, new_challenge)` for an expired seasonal challenge.
pub fn emit_challenge_expired(ctx: &ServerContext, char_id: i64, challenge_id: i64, new_kv: PyRep) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnChallengeExpired",
        "charid",
        vec![PyRep::Integer(challenge_id), new_kv],
    );
}

/// Emits `OnContextualOfferPurchased(offerID, purchaseInfo)` with real purchase metadata.
pub fn emit_contextual_offer_purchased(ctx: &ServerContext, char_id: i64, offer_id: i64) {
    if offer_id <= 0 {
        return;
    }
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnContextualOfferPurchased",
        "charid",
        vec![
            PyRep::Integer(offer_id),
            PyRep::Dict(vec![
                (PyRep::String("offerID".into()), PyRep::Integer(offer_id)),
                (PyRep::String("purchasedAt".into()), PyRep::Integer(now_ft)),
                (PyRep::String("confirmed".into()), PyRep::Bool(true)),
            ]),
        ],
    );
}

/// Emits `OnCrateLootSpawned()` when opening an inventory crate reveals its loot table.
pub fn emit_crate_loot_spawned(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCrateLootSpawned",
        "charid",
        vec![],
    );
}

/// Emits `OnFastCheckoutEnabledChanged(is_fast_checkout_enabled)` to characters on `user_id`.
pub fn emit_fast_checkout_enabled_changed(ctx: &ServerContext, user_id: u64, enabled: bool) {
    let char_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let mut ids: Vec<i64> = snap
            .characters
            .values()
            .filter(|c| c.user_id == user_id)
            .map(|c| c.character_id)
            .collect();
        ids.sort_unstable();
        ids
    });
    for cid in char_ids {
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnFastCheckoutEnabledChanged",
            "charid",
            vec![PyRep::Bool(enabled)],
        );
    }
}

/// Emits `OnFacilitiesUpdated(facilityIDs)` when facility taxes or settings change.
pub fn emit_facilities_updated(
    ctx: &ServerContext,
    solar_system_id: Option<u32>,
    char_id: i64,
    facility_id: i64,
) {
    let args = vec![PyRep::List(vec![PyRep::Integer(facility_id)])];
    if let Some(sys_id) = solar_system_id {
        ctx.notify(
            NotificationTarget::SolarSystem(sys_id),
            "OnFacilitiesUpdated",
            "solarsystemid",
            args,
        );
    } else {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnFacilitiesUpdated",
            "charid",
            args,
        );
    }
}

/// Emits `OnIndustryMaterials(jobID, materials)` when an industry job consumes non-empty input materials.
pub fn emit_industry_materials_consumed(
    ctx: &ServerContext,
    char_id: i64,
    job_id: i64,
    consumed: &[(u32, i32)],
) {
    if consumed.is_empty() {
        return;
    }
    let mats: Vec<PyRep> = consumed
        .iter()
        .filter(|&&(_, qty)| qty > 0)
        .map(|&(tid, qty)| {
            PyRep::keyval([
                ("typeID", PyRep::Integer(i64::from(tid))),
                ("quantity", PyRep::Integer(i64::from(qty))),
                ("missing", PyRep::Integer(0)),
            ])
        })
        .collect();
    if mats.is_empty() {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnIndustryMaterials",
        "charid",
        vec![PyRep::Integer(job_id), PyRep::List(mats)],
    );
}

/// Emits `OnLogonSkillsTrained(skillInfos, canTrain)` when offline skill queue catch-up finishes >= 1 skill.
pub fn emit_logon_skills_trained(
    ctx: &ServerContext,
    char_id: i64,
    trained: &[(u32, u8, u8, f64)],
    can_train: bool,
) {
    if trained.is_empty() {
        return;
    }
    let mut skill_map: HashMap<u32, (u8, u8, f64)> = HashMap::new();
    for &(tid, prev_lvl, new_lvl, sp) in trained {
        skill_map
            .entry(tid)
            .and_modify(|e| {
                e.1 = e.1.max(new_lvl);
                e.2 = e.2.max(sp);
            })
            .or_insert((prev_lvl, new_lvl, sp));
    }
    let mut keys: Vec<u32> = skill_map.keys().copied().collect();
    keys.sort_unstable();
    let entries: Vec<(PyRep, PyRep)> = keys
        .into_iter()
        .map(|tid| {
            let (prev_lvl, new_lvl, sp) = skill_map[&tid];
            (
                PyRep::Integer(i64::from(tid)),
                PyRep::keyval([
                    ("currentLevel", PyRep::Integer(i64::from(prev_lvl))),
                    ("skillLevel", PyRep::Integer(i64::from(new_lvl))),
                    ("skillPoints", PyRep::Real(sp)),
                ]),
            )
        })
        .collect();
    if entries.is_empty() {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnLogonSkillsTrained",
        "charid",
        vec![PyRep::Dict(entries), PyRep::Bool(can_train)],
    );
}

/// Emits `OnDestinationChange(solarSystemID, clearOtherWaypoints, first)` on ESI waypoint update.
pub fn emit_destination_change(
    ctx: &ServerContext,
    char_id: i64,
    destination_id: i64,
    clear_other_waypoints: bool,
    add_to_beginning: bool,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDestinationChange",
        "charid",
        vec![
            PyRep::Integer(destination_id),
            PyRep::Bool(clear_other_waypoints),
            PyRep::Bool(add_to_beginning),
        ],
    );
}

/// Emits `OnFleetRespawnPointsUpdate(respawnPoints)` to `fleet_id` when accessible clone bays/vats exist.
pub fn emit_fleet_respawn_points_if_any(ctx: &ServerContext, fleet_id: i64) {
    if fleet_id <= 0 {
        return;
    }
    let points: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
        let Some(fleet) = snap.fleets.get(&fleet_id) else {
            return Vec::new();
        };
        let boss_corp = snap
            .characters
            .get(&fleet.boss_id)
            .map(|c| c.corporation_id)
            .unwrap_or(0);
        let mut structure_ids: Vec<i64> = snap
            .structures
            .values()
            .filter(|st| {
                st.owner_corp_id == boss_corp && st.state != "destroyed" && st.state != "unanchored"
            })
            .map(|st| st.structure_id)
            .collect();
        structure_ids.sort_unstable();
        structure_ids
            .into_iter()
            .filter_map(|sid| {
                let st = snap.structures.get(&sid)?;
                Some(PyRep::keyval([
                    ("solarsystemID", PyRep::Integer(i64::from(st.solar_system_id))),
                    ("extraInfo", PyRep::Integer(st.structure_id)),
                    ("locationID", PyRep::Integer(st.structure_id)),
                    ("typeID", PyRep::Integer(i64::from(st.type_id))),
                ]))
            })
            .collect()
    });
    if points.is_empty() {
        return;
    }
    ctx.notify(
        NotificationTarget::Fleet(fleet_id),
        "OnFleetRespawnPointsUpdate",
        "fleetid",
        vec![PyRep::List(points)],
    );
}

/// Emits `OnFleetLootEvent(lootEvents)` (`{(charID, typeID): qty}`) when a fleet member loots from space.
pub fn emit_fleet_loot_event_if_in_fleet(
    ctx: &ServerContext,
    char_id: i64,
    fleet_id_opt: Option<i64>,
    looted_items: &[(u32, i32)],
) {
    let Some(fleet_id) = fleet_id_opt.filter(|&f| f > 0) else {
        return;
    };
    let mut counts: HashMap<u32, i64> = HashMap::new();
    for &(tid, qty) in looted_items {
        if qty > 0 {
            *counts.entry(tid).or_insert(0) += i64::from(qty);
        }
    }
    if counts.is_empty() {
        return;
    }
    let mut tids: Vec<u32> = counts.keys().copied().collect();
    tids.sort_unstable();
    let entries: Vec<(PyRep, PyRep)> = tids
        .into_iter()
        .map(|tid| {
            (
                PyRep::Tuple(vec![
                    PyRep::Integer(char_id),
                    PyRep::Integer(i64::from(tid)),
                ]),
                PyRep::Integer(counts[&tid]),
            )
        })
        .collect();
    ctx.notify(
        NotificationTarget::Fleet(fleet_id),
        "OnFleetLootEvent",
        "fleetid",
        vec![PyRep::Dict(entries)],
    );
}

/// Emits `OnAlphaInjectionAvailable()` when daily alpha injection becomes available for `char_id`.
pub fn emit_alpha_injection_available(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnAlphaInjectionAvailable",
        "charid",
        vec![],
    );
}

/// Emits `OnCCPNotification(title, subtitle, text, notificationTypeID, language, reference_id)`.
pub fn emit_ccp_notification(
    ctx: &ServerContext,
    target: NotificationTarget,
    title: &str,
    subtitle: &str,
    text: &str,
    notification_type_id: i64,
    reference_id: i64,
) {
    ctx.notify(
        target,
        "OnCCPNotification",
        "charid",
        vec![
            PyRep::String(title.to_string()),
            PyRep::String(subtitle.to_string()),
            PyRep::String(text.to_string()),
            PyRep::Integer(notification_type_id),
            PyRep::None,
            PyRep::Integer(reference_id),
        ],
    );
}

/// Broadcasts `OnCommunityFittingsUpdated()` when community fittings are modified.
pub fn emit_community_fittings_updated(ctx: &ServerContext) {
    ctx.notify(
        NotificationTarget::All,
        "OnCommunityFittingsUpdated",
        "clientID",
        vec![],
    );
}

/// Emits `OnLoginMilestoneAchieved(milestoneID, iskAmount, shouldStartAnimating)`.
pub fn emit_login_milestone_achieved(
    ctx: &ServerContext,
    char_id: i64,
    milestone_id: i64,
    isk_amount: f64,
    should_animate: bool,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnLoginMilestoneAchieved",
        "charid",
        vec![
            PyRep::Integer(milestone_id),
            PyRep::Real(isk_amount),
            PyRep::Bool(should_animate),
        ],
    );
}

/// Emits `OnMilestoneTimerStarted(timeLeft)`.
pub fn emit_milestone_timer_started(ctx: &ServerContext, char_id: i64, time_left: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnMilestoneTimerStarted",
        "charid",
        vec![PyRep::Integer(time_left)],
    );
}

/// Handles Batch 3 GM/QA slash commands (`/achievements reset`, `/chatban`, `/chatunban`,
/// `/campaign enroll`, `/shutdown cancel`, `/fastcheckout`, `/fwcapture`, `/spawnanomaly`).
pub fn handle_batch3_gm_slash_cmd(
    ctx: &ServerContext,
    caller_cid: i64,
    caller_sys: Option<u32>,
    cmd: &str,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let head = parts.first().copied()?.to_ascii_lowercase();
    match head.as_str() {
        "/achievements" if parts.get(1).is_some_and(|s| s.eq_ignore_ascii_case("reset")) => {
            let target_cid = parts
                .get(2)
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(caller_cid);
            ctx.quasar_gateway.progression.achievements.clear_character(target_cid as u32);
            ctx.db.with_mut_snapshot(|snap| {
                let mut ext = crate::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
                ext.achievements_by_char.remove(&target_cid);
                ext.achievement_counters_by_char.remove(&target_cid);
                crate::services::cosmetics_ui_ops::save_cosmetics_ui_state(snap, &ext);
            });
            ctx.notify(
                NotificationTarget::Character(target_cid),
                "OnAchievementsReset",
                "charid",
                vec![],
            );
            Some(Ok(PyRep::keyval([
                ("command", PyRep::String(cmd.to_string())),
                ("characterID", PyRep::Integer(target_cid)),
                ("reset", PyRep::Bool(true)),
            ])))
        }
        "/chatban" => {
            let Some(target_cid) = parts.get(1).and_then(|s| s.parse::<i64>().ok()) else {
                return Some(Err(BridgeRpcError::user_error("InvalidSlashArguments")));
            };
            let mode = parts.get(2).copied().unwrap_or("3600");
            let is_perm = mode.eq_ignore_ascii_case("perm") || mode.eq_ignore_ascii_case("permanent");
            let now_ft = ctx.now_blueos_filetime_ticks();
            let expiry_ft = if is_perm {
                i64::MAX
            } else {
                let secs = mode.parse::<i64>().unwrap_or(3600).max(1);
                now_ft.saturating_add(secs.saturating_mul(10_000_000))
            };
            ctx.db.with_mut_snapshot(|snap| {
                let mut bans: HashMap<i64, i64> = snap
                    .service_state_blobs
                    .get("gm_chat_bans")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                bans.insert(target_cid, expiry_ft);
                if let Ok(json) = serde_json::to_string(&bans) {
                    snap.service_state_blobs.insert("gm_chat_bans".into(), json);
                }
            });
            if is_perm {
                ctx.notify(
                    NotificationTarget::Character(target_cid),
                    "OnCharacterPermanentlyBanned",
                    "charid",
                    vec![],
                );
            } else {
                ctx.notify(
                    NotificationTarget::Character(target_cid),
                    "OnCharacterTemporarilyBanned",
                    "charid",
                    vec![PyRep::Integer(expiry_ft)],
                );
            }
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(target_cid)),
                ("permanent", PyRep::Bool(is_perm)),
                ("banExpiry", PyRep::Integer(expiry_ft)),
            ])))
        }
        "/chatunban" => {
            let Some(target_cid) = parts.get(1).and_then(|s| s.parse::<i64>().ok()) else {
                return Some(Err(BridgeRpcError::user_error("InvalidSlashArguments")));
            };
            ctx.db.with_mut_snapshot(|snap| {
                let mut bans: HashMap<i64, i64> = snap
                    .service_state_blobs
                    .get("gm_chat_bans")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                bans.remove(&target_cid);
                if let Ok(json) = serde_json::to_string(&bans) {
                    snap.service_state_blobs.insert("gm_chat_bans".into(), json);
                }
            });
            ctx.notify(
                NotificationTarget::Character(target_cid),
                "OnCharacterUnbanned",
                "charid",
                vec![],
            );
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(target_cid)),
                ("unbanned", PyRep::Bool(true)),
            ])))
        }
        "/campaign" if parts.get(1).is_some_and(|s| s.eq_ignore_ascii_case("enroll")) => {
            let campaign_id = parts.get(2).and_then(|s| s.parse::<i64>().ok()).unwrap_or(1);
            ctx.db.with_mut_snapshot(|snap| {
                let mut ext = crate::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
                ext.active_login_campaign_id = Some(campaign_id);
                ext.login_campaign_claimed_days.remove(&caller_cid);
                crate::services::cosmetics_ui_ops::save_cosmetics_ui_state(snap, &ext);
            });
            emit_campaign_enrollment_changed(ctx, caller_cid);
            Some(Ok(PyRep::keyval([
                ("campaignID", PyRep::Integer(campaign_id)),
                ("enrolled", PyRep::Bool(true)),
            ])))
        }
        "/shutdown" if parts.get(1).is_some_and(|s| s.eq_ignore_ascii_case("cancel")) => {
            let label = parts
                .get(2)
                .copied()
                .unwrap_or("UI/Shared/ClusterShutdownDelayed");
            {
                let mut st = ctx.cluster_state.write().unwrap_or_else(|e| e.into_inner());
                st.shutdown_in_progress = false;
            }
            emit_cluster_shutdown_cancelled(ctx, label);
            Some(Ok(PyRep::keyval([
                ("cancelled", PyRep::Bool(true)),
                ("explanationLabel", PyRep::String(label.to_string())),
            ])))
        }
        "/fastcheckout" => {
            let enabled = parts
                .get(1)
                .map(|s| *s == "1" || s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("on"))
                .unwrap_or(true);
            let uid = ctx
                .db
                .with_snapshot(|s| s.characters.get(&caller_cid).map(|c| c.user_id))
                .unwrap_or(0);
            if uid > 0 {
                let prev = crate::services::fast_checkout_and_activity::get_user_fast_checkout_state(ctx, uid).enabled;
                if prev != enabled {
                    crate::services::fast_checkout_and_activity::set_user_fast_checkout_enabled(ctx, uid, enabled);
                    emit_fast_checkout_enabled_changed(ctx, uid, enabled);
                }
            }
            Some(Ok(PyRep::Bool(enabled)))
        }
        "/spawnanomaly" => {
            let sys_id = caller_sys.unwrap_or(30_000_142);
            let dungeon_id = parts.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(10_001);
            let (anom_id, inst_id) =
                super::notify_emitters_batch3_space::spawn_and_emit_cosmic_anomaly(ctx, sys_id, dungeon_id);
            Some(Ok(PyRep::keyval([
                ("targetID", PyRep::String(anom_id)),
                ("instanceID", PyRep::Integer(inst_id)),
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
            ])))
        }
        _ => None,
    }
}
