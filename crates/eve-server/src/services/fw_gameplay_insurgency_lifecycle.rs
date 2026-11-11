//! Insurgency campaign spawning, stargate spread, Ambition victory resolution, and tick lifecycle.

#![forbid(unsafe_code)]

use super::{
    FACTION_ANGEL_CARTEL, FACTION_GURISTAS, INSURGENCY_MAX_PAYOUT_LP,
    PIRATE_FOB_STRUCTURE_BASE_ID, SURRENDER_IHUB_VULNERABILITY_DELAY_SECS,
};
use crate::router::{NotificationTarget, ServerContext};
use crate::services::fw_gameplay::militia_lp_corp_id;
use crate::services::fw_services::{
    compute_all_frontline_statuses, set_facwar_bunker_vulnerable, STATUS_FRONTLINE,
};
use crate::services::insurgency_services::{
    emit_campaign_started, end_insurgency_campaign, get_campaign_phase_and_timer,
    init_insurgency_campaign, insurgency_stage, spawn_pirate_stronghold, with_insurgency_runtime,
    PHASE_ACTIVE, PHASE_FORECASTING, PHASE_WINDDOWN, WINDDOWN_DURATION_SECS,
};
use eve_destiny::{Ball, MovementMode};
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

/// Ensures the unified Pirate FOB (`79_172` Guristas / `78_260` Angel Cartel) `UpwellStructureRecord`,
/// `PirateStronghold` site record, space ball with `CombatState`, and NPC entity exist in `fob_solar_system_id`.
pub async fn ensure_campaign_pirate_fob(ctx: &ServerContext, campaign_id: i64) -> Option<i64> {
    let (fob_sys, pirate_fac, fob_id, fob_type, pos, is_destroyed) = ctx.db.with_mut_snapshot(|snap| {
        crate::services::insurgency_services::ensure_unified_pirate_fob_in_snap(
            snap,
            Some(&ctx.sde),
            campaign_id,
        )
    })?;
    if is_destroyed {
        return None;
    }
    let sys_handle = ctx.systems.get_or_spawn(fob_sys);
    if sys_handle.get_ball_snapshot(fob_id).await.ok().flatten().is_none() {
        let pos_vec = DVec3::from_array(pos);
        let ball = Ball {
            id: fob_id,
            type_id: fob_type,
            position: pos_vec,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 5.0e8,
            radius: 1_200.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        };
        let mut cs = ctx.build_type_combat_state(fob_type);
        cs.max_shield_hp = 2_500.0;
        cs.shield_hp = 2_500.0;
        cs.max_armor_hp = 2_500.0;
        cs.armor_hp = 2_500.0;
        cs.max_structure_hp = 3_500.0;
        cs.structure_hp = 3_500.0;
        ctx.db.with_snapshot(|snap| {
            if let Some(st) = snap.structures.get(&fob_id) {
                cs.shield_hp = st.shield_hp;
                cs.armor_hp = st.armor_hp;
                cs.structure_hp = st.hull_hp;
            }
        });
        let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;
        let entity = crate::services::combat_crimewatch::ExtendedNpcEntity {
            ball_id: fob_id,
            type_id: fob_type,
            faction_id: pirate_fac as u32,
            archetype: "PirateStronghold".into(),
            solar_system_id: fob_sys,
            spawn_group_id: campaign_id as u32,
            target_id: None,
            aggression_threat: HashMap::new(),
            threat_table: HashMap::new(),
            bounty_isk: 30_000_000.0,
            optimal_range: 50_000.0,
            orbit_range: 30_000.0,
            damage_vector: eve_destiny::DamageVector {
                em: 50.0,
                explosive: 50.0,
                kinetic: 50.0,
                thermal: 50.0,
            },
            entropic_cycles: 0,
            drifter_overshield_hp: 0.0,
            drifter_doomsday_fired: false,
            web_speed_factor: 0.0,
            web_range: 0.0,
            scram_strength: 0.0,
            scram_range: 0.0,
            neut_amount: 0.0,
            ecm_jammed: false,
            remote_shield_rep: 0.0,
            remote_armor_rep: 0.0,
            remote_rep_range: 0.0,
            tag_drop_type_id: None,
            spawn_position: pos_vec,
            sleeper_wh_class: 0,
            custom_cargo_loot: None,
        };
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            rt.npc_entities.insert(fob_id, entity);
        });
    }
    Some(fob_id)
}

/// Spreads an active Pirate Insurgency when `source_sys_id` reaches Corruption Stage 3 (`>= 3`)
/// (unless `source_sys_id` is Stage 5 Suppressed), adding a stargate-adjacent `0.0..=0.8` Empire system.
pub fn try_spread_insurgency_from_system(
    ctx: &ServerContext,
    campaign_id: i64,
    source_sys_id: u32,
) -> Option<u32> {
    let (covered, suppressed5) = with_insurgency_runtime(ctx, |rt| {
        rt.campaigns.get(&campaign_id).map(|c| {
            let s5: std::collections::HashSet<u32> = c
                .suppression_points
                .iter()
                .filter(|(_, &pts)| insurgency_stage(pts) >= 5)
                .map(|(&sid, _)| sid)
                .collect();
            (c.systems.clone(), s5)
        })
    })?;
    let source_stage5_suppressed = suppressed5.contains(&source_sys_id)
        || ctx.db.with_snapshot(|snap| {
            snap.system_states
                .get(&source_sys_id)
                .is_some_and(|st| st.insurgency_suppression_stage >= 5)
        });
    if source_stage5_suppressed {
        return None;
    }

    let valid_sec = |sys_id: u32| -> bool {
        ctx.sde.solar_systems.get(&sys_id).is_some_and(|s| {
            s.solar_system_id < 30_100_000
                && matches!(s.wormhole_class_id, None | Some(7..=9))
                && s.security_status >= 0.0
                && s.security_status < 0.85
                && ctx.sde.regions.get(&s.region_id).is_some_and(|r| {
                    matches!(r.faction_id, Some(500_001..=500_004 | 500_007 | 500_008))
                })
        })
    };
    let has_hs_covered = covered.iter().any(|&s| {
        ctx.sde
            .solar_systems
            .get(&s)
            .is_some_and(|sys| sys.security_status >= 0.5)
    });
    let wz_systems: std::collections::HashSet<u32> = ctx
        .db
        .with_snapshot(|snap| snap.fw_system_occupations.keys().copied().collect());
    let rank = |sys_id: u32| -> (u8, u8, u32) {
        let is_hs = ctx
            .sde
            .solar_systems
            .get(&sys_id)
            .is_some_and(|s| s.security_status >= 0.5);
        let hs_prio = if !has_hs_covered && is_hs { 0 } else { 1 };
        let wz_prio = if wz_systems.contains(&sys_id) { 0 } else { 1 };
        (hs_prio, wz_prio, sys_id)
    };

    let mut candidate: Option<u32> = ctx
        .sde
        .jumps
        .values()
        .filter(|j| j.from_solar_system_id == source_sys_id)
        .map(|j| j.to_solar_system_id)
        .filter(|s| !covered.contains(s) && valid_sec(*s))
        .min_by_key(|&s| rank(s));

    if candidate.is_none() {
        let state_s5: std::collections::HashSet<u32> = ctx.db.with_snapshot(|snap| {
            covered
                .iter()
                .copied()
                .filter(|s| {
                    suppressed5.contains(s)
                        || snap
                            .system_states
                            .get(s)
                            .is_some_and(|st| st.insurgency_suppression_stage >= 5)
                })
                .collect()
        });
        candidate = ctx
            .sde
            .jumps
            .values()
            .filter(|j| {
                covered.contains(&j.from_solar_system_id)
                    && !state_s5.contains(&j.from_solar_system_id)
            })
            .map(|j| j.to_solar_system_id)
            .filter(|s| !covered.contains(s) && valid_sec(*s))
            .min_by_key(|&s| rank(s));
    }

    let new_sys = candidate?;
    with_insurgency_runtime(ctx, |rt| {
        if let Some(c) = rt.campaigns.get_mut(&campaign_id) {
            if !c.systems.contains(&new_sys) {
                c.systems.push(new_sys);
                c.corruption_points.entry(new_sys).or_insert(0);
                c.suppression_points.entry(new_sys).or_insert(0);
            }
        }
    });
    let now_spread = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::security_mgr::sync_stage_gated_insurgency_sites_in_snap(
            &ctx.sde,
            snap,
            new_sys,
            now_spread,
        );
    });
    emit_campaign_started(ctx, new_sys, campaign_id);
    Some(new_sys)
}

#[derive(Debug, Clone)]
pub struct CampaignResolutionEffects {
    pub campaign_id: i64,
    pub outcome: String,
    pub new_p_mod: u8,
    pub new_e_mod: u8,
    pub fob_sys: u32,
    pub sys_list: Vec<u32>,
    pub pirate_fac: i64,
    pub payouts: Vec<(i64, i64, i64)>,
    pub updated_balances: Vec<(i64, f64)>,
}

fn fallback_militia_lp_corp(fid: i64) -> i64 {
    match fid {
        500_001 => 1_000_180,
        500_002 => 1_000_179,
        500_003 => 1_000_182,
        500_004 => 1_000_181,
        500_010 => 1_000_436,
        500_011 => 1_000_437,
        _ => 1_000_127,
    }
}

/// Resolves an Insurgency campaign inside `WorldStateSnapshot`, updating Ambition modifiers,
/// `campaign_outcomes`, surrender I-Hub vulnerability, LP/ISK payouts, and journal entries.
pub fn resolve_insurgency_campaign_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    sde_opt: Option<&eve_sde::SdeRegistry>,
    campaign_id: i64,
    forced_outcome: Option<&str>,
    now: u64,
) -> Result<CampaignResolutionEffects, BridgeRpcError> {
    let camp = snap
        .insurgency_runtime
        .campaigns
        .get_mut(&campaign_id)
        .ok_or_else(|| BridgeRpcError {
            code: "CampaignNotFound".into(),
            message: format!("Insurgency campaign {campaign_id} not found"),
        })?;
    let pirate_fac = camp.pirate_faction_id;
    let fob_sys = camp.fob_solar_system_id;
    let sys_list = camp.systems.clone();
    if let Some(existing) = snap.insurgency_runtime.campaign_outcomes.get(&campaign_id) {
        let p = snap
            .insurgency_runtime
            .pirate_ambition_modifier
            .get(&pirate_fac)
            .copied()
            .unwrap_or(0);
        let e = snap
            .insurgency_runtime
            .empire_ambition_modifier
            .get(&pirate_fac)
            .copied()
            .unwrap_or(0);
        return Ok(CampaignResolutionEffects {
            campaign_id,
            outcome: existing.clone(),
            new_p_mod: p,
            new_e_mod: e,
            fob_sys,
            sys_list,
            pirate_fac,
            payouts: Vec::new(),
            updated_balances: Vec::new(),
        });
    }
    let mut p_mod = snap
        .insurgency_runtime
        .pirate_ambition_modifier
        .get(&pirate_fac)
        .copied()
        .unwrap_or(0)
        .min(5);
    let mut e_mod = snap
        .insurgency_runtime
        .empire_ambition_modifier
        .get(&pirate_fac)
        .copied()
        .unwrap_or(0)
        .min(5);
    let p_target = usize::from(5 + p_mod);
    let e_target = usize::from(5 + e_mod);
    let corrupted_5 = camp
        .corruption_points
        .values()
        .filter(|&&pts| insurgency_stage(pts) >= 5)
        .count();
    let suppressed_5 = camp
        .suppression_points
        .values()
        .filter(|&&pts| insurgency_stage(pts) >= 5)
        .count();

    let outcome = if let Some(forced) = forced_outcome {
        forced.to_string()
    } else if corrupted_5 >= p_target {
        "PirateWin".to_string()
    } else if suppressed_5 >= e_target {
        "EmpireWin".to_string()
    } else {
        "Tie".to_string()
    };

    match outcome.as_str() {
        "PirateWin" => {
            if e_mod > 0 {
                e_mod -= 1;
            } else {
                p_mod = (p_mod + 1).min(5);
            }
        }
        "EmpireWin" => {
            if p_mod > 0 {
                p_mod -= 1;
            } else {
                e_mod = (e_mod + 1).min(5);
            }
        }
        _ => {}
    }

    if camp.active_end_unix > now {
        camp.forecasting_end_unix = camp.forecasting_end_unix.min(now);
        camp.active_end_unix = now;
        camp.winddown_end_unix = now + WINDDOWN_DURATION_SECS;
    }

    snap.insurgency_runtime
        .pirate_ambition_modifier
        .insert(pirate_fac, p_mod);
    snap.insurgency_runtime
        .empire_ambition_modifier
        .insert(pirate_fac, e_mod);
    snap.insurgency_runtime
        .campaign_outcomes
        .insert(campaign_id, outcome.clone());
    if outcome == "PirateWin" {
        snap.insurgency_runtime
            .surrender_ihub_vulnerable_unix
            .insert(fob_sys, now + SURRENDER_IHUB_VULNERABILITY_DELAY_SECS);
        if let Some(occ) = snap.fw_system_occupations.get_mut(&fob_sys) {
            occ.occupier_faction_id = pirate_fac;
        }
        snap.fw_runtime.ihub_vulnerable.insert(fob_sys, false);
    }

    let contribs = snap
        .insurgency_runtime
        .campaign_contributions
        .get(&campaign_id)
        .cloned()
        .unwrap_or_default();
    let pirate_total: i64 = contribs
        .iter()
        .filter(|(_, fid, _)| *fid == pirate_fac || *fid == FACTION_GURISTAS || *fid == FACTION_ANGEL_CARTEL)
        .map(|(_, _, pts)| *pts)
        .sum();
    let empire_total: i64 = contribs
        .iter()
        .filter(|(_, fid, _)| *fid != pirate_fac && *fid != FACTION_GURISTAS && *fid != FACTION_ANGEL_CARTEL)
        .map(|(_, _, pts)| *pts)
        .sum();

    let mut payouts = Vec::with_capacity(contribs.len());
    let mut updated_balances = Vec::new();
    for (cid, fid, pts) in contribs {
        let is_pirate = fid == pirate_fac || fid == FACTION_GURISTAS || fid == FACTION_ANGEL_CARTEL;
        let side_total = if is_pirate { pirate_total } else { empire_total }.max(1);
        let share = (pts as f64) / (side_total as f64);
        let scaled_ratio = (share / 0.45).clamp(0.0, 1.0);
        let side_mult = match (outcome.as_str(), is_pirate) {
            ("PirateWin", true) | ("EmpireWin", false) => 1.0,
            ("PirateWin", false) | ("EmpireWin", true) => 0.4,
            _ => 0.0,
        };
        let lp_award = ((INSURGENCY_MAX_PAYOUT_LP as f64) * scaled_ratio * side_mult)
            .round()
            .min(INSURGENCY_MAX_PAYOUT_LP as f64) as i64;
        payouts.push((cid, fid, lp_award));
        if lp_award > 0 {
            let lp_corp = sde_opt
                .map(|sde| militia_lp_corp_id(sde, fid))
                .unwrap_or_else(|| fallback_militia_lp_corp(fid));
            let isk_award = lp_award.saturating_mul(100);
            if let Some(ch) = snap.characters.get_mut(&cid) {
                *ch.loyalty_points.entry(lp_corp).or_insert(0) += lp_award;
                let isk_cents = eve_db::IskCents::from_isk(isk_award);
                ch.balance = ch.balance.checked_add(isk_cents).unwrap_or(ch.balance);
                let new_bal = ch.balance;
                snap.append_journal(
                    cid,
                    "InsurgencyContributionPayout",
                    isk_cents,
                    new_bal,
                    format!("Insurgency campaign {campaign_id} contribution reward"),
                );
                updated_balances.push((cid, new_bal.to_wire_f64()));
            }
        }
    }

    Ok(CampaignResolutionEffects {
        campaign_id,
        outcome,
        new_p_mod: p_mod,
        new_e_mod: e_mod,
        fob_sys,
        sys_list,
        pirate_fac,
        payouts,
        updated_balances,
    })
}

/// Broadcasts cash, campaign update, and state-change notifications after resolving a campaign.
pub fn emit_campaign_resolution_effects_core(
    db: &eve_db::WorldRepository,
    gw: Option<&crate::quasar_gateway::PublicGatewayRouter>,
    pending_notifications: Option<
        &std::sync::RwLock<
            HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>,
        >,
    >,
    notify_tx: Option<
        &tokio::sync::broadcast::Sender<(
            i64,
            crate::router::cache_notifications::PendingMachoNotification,
        )>,
    >,
    res: &CampaignResolutionEffects,
) {
    let now_unix = db.clock().now_unix_secs();
    let now_ft = db.clock().now_blueos_filetime_ticks();
    for &(cid, new_bal) in &res.updated_balances {
        let (pkt, mpkt) = crate::router::ServerContext::build_macho_notification_packet(
            cid,
            None,
            "OnAccountChange",
            "cash",
            vec![
                PyRep::String("cash".into()),
                PyRep::Integer(cid),
                PyRep::Real(new_bal),
            ],
        );
        let notif = crate::router::cache_notifications::PendingMachoNotification {
            seq_id: crate::router::cache_notifications::next_notification_seq_id(),
            recipient_char_id: cid,
            event: "OnAccountChange".into(),
            id_type: "cash".into(),
            args: vec![
                PyRep::String("cash".into()),
                PyRep::Integer(cid),
                PyRep::Real(new_bal),
            ],
            packet: pkt,
            marshaled_packet: mpkt,
            created_filetime: now_ft,
        };
        if let Some(pend) = pending_notifications {
            if let Ok(mut map) = pend.write() {
                map.entry(cid).or_default().push(notif.clone());
            }
        }
        if let Some(tx) = notify_tx {
            let _ = tx.send((cid, notif));
        }
    }
    let outcome_state = match res.outcome.as_str() {
        "PirateWin" => crate::services::insurgency_services::CAMPAIGN_STATE_PIRATE_WIN,
        "EmpireWin" | "AntiPirateWin" => {
            crate::services::insurgency_services::CAMPAIGN_STATE_ANTIPIRATE_WIN
        }
        _ => crate::services::insurgency_services::CAMPAIGN_STATE_NO_WINNER,
    };
    let state_args = vec![
        PyRep::Integer(outcome_state),
        PyRep::Integer(res.pirate_fac),
        PyRep::Integer(i64::from(res.fob_sys)),
    ];
    for &sys in &res.sys_list {
        if let Some(router) = gw {
            let mut pb = Vec::new();
            eve_proto::quasar::encode_varint_field(1, res.campaign_id as u64, &mut pb);
            eve_proto::quasar::encode_varint_field(2, 5, &mut pb);
            let _ = router.publish_notice(
                crate::quasar_gateway::NoticeScope::SolarSystem(sys),
                eve_proto::quasar::GoogleAny {
                    type_url: format!(
                        "{}eve_public.pirate.insurgency.campaign.UpdateNotice",
                        eve_proto::quasar::TYPE_GOOGLEAPIS_PREFIX
                    ),
                    value: pb,
                },
                now_unix,
            );
        }
        let system_chars: Vec<i64> = db.with_snapshot(|snap| {
            snap.characters
                .values()
                .filter(|c| c.solar_system_id == sys)
                .map(|c| c.character_id)
                .collect()
        });
        let update_args = db.with_snapshot(|snap| {
            let rt = &snap.insurgency_runtime;
            rt.campaigns
                .get(&res.campaign_id)
                .map(|c| {
                    let emp_mod = rt
                        .empire_ambition_modifier
                        .get(&c.pirate_faction_id)
                        .copied()
                        .unwrap_or(0);
                    vec![
                        crate::services::insurgency_services::build_campaign_snapshot_for_system(
                            c,
                            sys,
                            now_unix,
                            emp_mod,
                        ),
                    ]
                })
                .unwrap_or_else(|| vec![PyRep::Integer(res.campaign_id), PyRep::Integer(5)])
        });
        for cid in system_chars {
            let (pkt, mpkt) = crate::router::ServerContext::build_macho_notification_packet(
                cid,
                None,
                "OnInsurgencyCampaignUpdatedForLocation",
                "solarsystemid",
                update_args.clone(),
            );
            let notif1 = crate::router::cache_notifications::PendingMachoNotification {
                seq_id: crate::router::cache_notifications::next_notification_seq_id(),
                recipient_char_id: cid,
                event: "OnInsurgencyCampaignUpdatedForLocation".into(),
                id_type: "solarsystemid".into(),
                args: update_args.clone(),
                packet: pkt,
                marshaled_packet: mpkt,
                created_filetime: now_ft,
            };
            let (pkt2, mpkt2) = crate::router::ServerContext::build_macho_notification_packet(
                cid,
                None,
                "OnInsurgencyStateChangedForFaction",
                "solarsystemid",
                state_args.clone(),
            );
            let notif2 = crate::router::cache_notifications::PendingMachoNotification {
                seq_id: crate::router::cache_notifications::next_notification_seq_id(),
                recipient_char_id: cid,
                event: "OnInsurgencyStateChangedForFaction".into(),
                id_type: "solarsystemid".into(),
                args: state_args.clone(),
                packet: pkt2,
                marshaled_packet: mpkt2,
                created_filetime: now_ft,
            };
            if let Some(pend) = pending_notifications {
                if let Ok(mut map) = pend.write() {
                    let list = map.entry(cid).or_default();
                    list.push(notif1.clone());
                    list.push(notif2.clone());
                }
            }
            if let Some(tx) = notify_tx {
                let _ = tx.send((cid, notif1));
                let _ = tx.send((cid, notif2));
            }
        }
    }
}

/// Evaluates an Insurgency campaign's Ambition-scaled victory condition (`5 + ambition_modifier` Stage-5 systems),
/// updates Pirate/Empire Ambition modifiers (`0..=5`, mutually exclusive; underdog win reduces leader's modifier by 1),
/// schedules post-surrender FW I-Hub vulnerability (`25h`) on a Pirate victory, and pays personal contribution-scaled
/// ISK + militia-corp LP (`1.0x` winner, `0.4x` loser, max at `45%` share, capped at `1,500,000 LP`, `0` on tie).
pub fn evaluate_and_resolve_insurgency_campaign(
    ctx: &ServerContext,
    campaign_id: i64,
    forced_outcome: Option<&str>,
) -> Result<(String, u8, u8, Vec<(i64, i64, i64)>), BridgeRpcError> {
    let now = ctx.now_unix();
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        resolve_insurgency_campaign_in_snap(snap, Some(&ctx.sde), campaign_id, forced_outcome, now)
    })?;

    if res.outcome == "PirateWin" {
        let _ = spawn_pirate_stronghold(ctx, campaign_id);
    } else if res.outcome == "EmpireWin" || res.outcome == "AntiPirateWin" {
        let _ = crate::services::cluster_macho_misc_services::trigger_zarzakh_horizon_siege(
            ctx,
            Some(campaign_id),
            crate::services::cluster_macho_misc_services::HORIZON_SIEGE_ACTIVE_SECS,
        );
    }

    emit_campaign_resolution_effects_core(
        &ctx.db,
        Some(&ctx.quasar_gateway),
        Some(ctx.pending_notifications.as_ref()),
        Some(&ctx.notify_tx),
        &res,
    );

    Ok((res.outcome, res.new_p_mod, res.new_e_mod, res.payouts))
}

/// Live 1 Hz Pirate Insurgency campaign scheduler (`Forecasting` -> `Active` -> Ambition / Tie -> `WindDown` -> End).
pub async fn tick_insurgency_campaigns(ctx: &ServerContext, now_unix: u64) {
    let _ = crate::services::cluster_macho_misc_services::tick_zarzakh_horizon_siege(ctx, now_unix);
    let has_due_surrender = ctx.db.with_snapshot(|snap| {
        snap.insurgency_runtime
            .surrender_ihub_vulnerable_unix
            .iter()
            .any(|(&sys, &ts)| now_unix >= ts && !snap.fw_runtime.ihub_vulnerable.get(&sys).copied().unwrap_or(false))
    });
    let unlocked_surrender_sys: Vec<(u32, i64)> = if has_due_surrender {
        ctx.db.with_mut_snapshot(|snap| {
            let due: Vec<u32> = snap
                .insurgency_runtime
                .surrender_ihub_vulnerable_unix
                .iter()
                .filter(|(&sys, &ts)| now_unix >= ts && !snap.fw_runtime.ihub_vulnerable.get(&sys).copied().unwrap_or(false))
                .map(|(&sys, _)| sys)
                .collect();
            let mut out = Vec::new();
            for sys in due {
                if let Some(occ) = snap.fw_system_occupations.get_mut(&sys) {
                    if !(500_001..=500_004).contains(&occ.occupier_faction_id) {
                        occ.occupier_faction_id = occ.owner_faction_id;
                    }
                    occ.victory_points = occ.victory_points_threshold;
                    let occ_fac = occ.occupier_faction_id;
                    snap.fw_runtime
                        .system_victory_points
                        .insert(sys, occ.victory_points_threshold);
                    if let Some(st) = snap.system_states.get_mut(&sys) {
                        st.fw_contested_pct = 100.0;
                    }
                    out.push((sys, occ_fac));
                }
            }
            out
        })
    } else {
        Vec::new()
    };
    for (sys, occ_fac) in unlocked_surrender_sys {
        set_facwar_bunker_vulnerable(ctx, sys, occ_fac, true);
    }

    let auto_frontline_seed: Option<(i64, i64, i64, u32, Vec<u32>)> = ctx.db.with_snapshot(|snap| {
        if !snap.insurgency_runtime.campaigns.is_empty() || snap.fw_system_occupations.is_empty() {
            return None;
        }
        let mut pirate_pilots: Vec<_> = snap
            .characters
            .values()
            .filter(|c| {
                matches!(c.war_faction_id, Some(FACTION_GURISTAS) | Some(FACTION_ANGEL_CARTEL))
                    && snap.fw_system_occupations.contains_key(&c.solar_system_id)
            })
            .collect();
        pirate_pilots.sort_by_key(|c| c.character_id);
        let pirate_pilot = pirate_pilots.first()?;
        let pirate_fac = pirate_pilot.war_faction_id?;
        let frontlines = compute_all_frontline_statuses(ctx, snap);
        let frontline_sys = if frontlines.get(&pirate_pilot.solar_system_id) == Some(&STATUS_FRONTLINE) {
            pirate_pilot.solar_system_id
        } else {
            *frontlines.iter().find(|(_, &st)| st == STATUS_FRONTLINE)?.0
        };
        let target_fac = snap
            .fw_system_occupations
            .get(&frontline_sys)
            .map(|o| o.occupier_faction_id)
            .unwrap_or(500_001);
        let mut starting = vec![frontline_sys];
        for _pass in 0..3 {
            if starting.len() >= 4 {
                break;
            }
            let frontier = starting.clone();
            let mut adj: Vec<u32> = ctx
                .sde
                .jumps
                .values()
                .filter(|j| frontier.contains(&j.from_solar_system_id))
                .map(|j| j.to_solar_system_id)
                .filter(|s| {
                    !starting.contains(s)
                        && ctx
                            .sde
                            .solar_systems
                            .get(s)
                            .is_some_and(|sys| sys.security_status > 0.0 && sys.security_status <= 0.8 && sys.solar_system_id < 31_000_000)
                })
                .collect();
            adj.sort_by_key(|s| (if snap.fw_system_occupations.contains_key(s) { 0u8 } else { 1u8 }, *s));
            adj.dedup();
            for s in adj {
                if starting.len() >= 4 {
                    break;
                }
                if !starting.contains(&s) {
                    starting.push(s);
                }
            }
        }
        let next_cid = (now_unix % 100_000) as i64 + 900_001;
        Some((next_cid, pirate_fac, target_fac, frontline_sys, starting))
    });
    if let Some((cid, p_fac, t_fac, fl_sys, systems)) = auto_frontline_seed {
        init_insurgency_campaign(ctx, cid, p_fac, t_fac, fl_sys, systems.clone());
        with_insurgency_runtime(ctx, |rt| {
            rt.insurgency_instances
                .retain(|s| !systems.contains(&s.solar_system_id) || s.site_type == "PirateStronghold");
        });
    }

    let camp_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.keys().copied().collect()
    });
    for cid in camp_ids {
        let _ = ensure_campaign_pirate_fob(ctx, cid).await;

        let (action, notify_active) = {
            let (needs_active_notify, action_ro) = ctx.db.with_snapshot(|snap| {
                let Some(camp) = snap.insurgency_runtime.campaigns.get(&cid) else {
                    return (false, None);
                };
                let (phase, _) = get_campaign_phase_and_timer(camp, now_unix);
                if now_unix >= camp.winddown_end_unix {
                    return (false, Some("End"));
                }
                if phase == PHASE_FORECASTING {
                    return (false, None);
                }
                let needs_notify =
                    phase == PHASE_ACTIVE && !snap.insurgency_runtime.active_state_notified.contains(&cid);
                if phase == PHASE_ACTIVE {
                    if !snap.insurgency_runtime.campaign_outcomes.contains_key(&cid) {
                        let p_mod = snap
                            .insurgency_runtime
                            .pirate_ambition_modifier
                            .get(&camp.pirate_faction_id)
                            .copied()
                            .unwrap_or(0)
                            .min(5);
                        let c5 = camp
                            .corruption_points
                            .values()
                            .filter(|&&pts| insurgency_stage(pts) >= 5)
                            .count();
                        let fob_destroyed = snap
                            .structures
                            .get(&(PIRATE_FOB_STRUCTURE_BASE_ID + cid))
                            .is_some_and(|st| st.state == "Destroyed");
                        if fob_destroyed {
                            return (needs_notify, Some("ResolveEmpireWin"));
                        }
                        if c5 >= usize::from(5 + p_mod) {
                            return (needs_notify, Some("Resolve"));
                        }
                    }
                    return (needs_notify, None);
                }
                if !snap.insurgency_runtime.campaign_outcomes.contains_key(&cid) {
                    let fob_destroyed = snap
                        .structures
                        .get(&(PIRATE_FOB_STRUCTURE_BASE_ID + cid))
                        .is_some_and(|st| st.state == "Destroyed");
                    if fob_destroyed {
                        return (needs_notify, Some("ResolveEmpireWin"));
                    }
                    return (needs_notify, Some("Resolve"));
                }
                (needs_notify, None)
            });
            if !needs_active_notify {
                (action_ro, None)
            } else {
                ctx.db.with_mut_snapshot(|snap| {
                    let Some(camp) = snap.insurgency_runtime.campaigns.get(&cid).cloned() else {
                        return (None, None);
                    };
                    let (phase, _) = get_campaign_phase_and_timer(&camp, now_unix);
                    if now_unix >= camp.winddown_end_unix {
                        return (Some("End"), None);
                    }
                    if phase == PHASE_FORECASTING {
                        return (None, None);
                    }
                    let notify_active = if phase == PHASE_ACTIVE && snap.insurgency_runtime.active_state_notified.insert(cid) {
                        Some((camp.systems.clone(), camp.pirate_faction_id, camp.fob_solar_system_id))
                    } else {
                        None
                    };
                    if phase == PHASE_ACTIVE {
                        if notify_active.is_some() {
                            for &sys in &camp.systems {
                                crate::services::security_mgr::sync_stage_gated_insurgency_sites_in_snap(
                                    &ctx.sde,
                                    snap,
                                    sys,
                                    now_unix,
                                );
                            }
                        }
                        if !snap.insurgency_runtime.campaign_outcomes.contains_key(&cid) {
                            let p_mod = snap
                                .insurgency_runtime
                                .pirate_ambition_modifier
                                .get(&camp.pirate_faction_id)
                                .copied()
                                .unwrap_or(0)
                                .min(5);
                            let _e_mod = snap
                                .insurgency_runtime
                                .empire_ambition_modifier
                                .get(&camp.pirate_faction_id)
                                .copied()
                                .unwrap_or(0)
                                .min(5);
                            let c5 = camp
                                .corruption_points
                                .values()
                                .filter(|&&pts| insurgency_stage(pts) >= 5)
                                .count();
                            let _s5 = camp
                                .suppression_points
                                .values()
                                .filter(|&&pts| insurgency_stage(pts) >= 5)
                                .count();
                            let fob_destroyed = snap
                                .structures
                                .get(&(PIRATE_FOB_STRUCTURE_BASE_ID + cid))
                                .is_some_and(|st| st.state == "Destroyed");
                            if fob_destroyed {
                                return (Some("ResolveEmpireWin"), notify_active);
                            }
                            if c5 >= usize::from(5 + p_mod) {
                                return (Some("Resolve"), notify_active);
                            }
                        }
                        return (None, notify_active);
                    }
                    if !snap.insurgency_runtime.campaign_outcomes.contains_key(&cid) {
                        let fob_destroyed = snap
                            .structures
                            .get(&(PIRATE_FOB_STRUCTURE_BASE_ID + cid))
                            .is_some_and(|st| st.state == "Destroyed");
                        if fob_destroyed {
                            return (Some("ResolveEmpireWin"), notify_active);
                        }
                        return (Some("Resolve"), notify_active);
                    }
                    (None, notify_active)
                })
            }
        };

        let active_sys_info: Option<(i64, i64, Vec<(u32, u8, u8)>)> = ctx.db.with_snapshot(|snap| {
            let camp = snap.insurgency_runtime.campaigns.get(&cid)?;
            let (phase, _) = get_campaign_phase_and_timer(camp, now_unix);
            if !matches!(phase, PHASE_ACTIVE | PHASE_WINDDOWN) {
                return None;
            }
            let list = camp
                .systems
                .iter()
                .map(|&sys| {
                    let st = snap.system_states.get(&sys);
                    (
                        sys,
                        st.map_or(0, |s| s.insurgency_corruption_stage),
                        st.map_or(0, |s| s.insurgency_suppression_stage),
                    )
                })
                .collect();
            Some((camp.pirate_faction_id, camp.target_faction_id, list))
        });
        if let Some((p_fac, e_fac, sys_stages)) = active_sys_info {
            for (sys, c_st, s_st) in sys_stages {
                super::sync_insurgency_system_npcs(ctx, p_fac, e_fac, sys, c_st, s_st).await;
            }
            crate::services::insurgency_complex_ops::sync_insurgency_complex_sites_and_facilities(ctx, now_unix).await;
        }

        if let Some((systems, p_fac, fob_sys)) = notify_active {
            for sys in systems {
                ctx.notify(
                    NotificationTarget::SolarSystem(sys),
                    "OnInsurgencyStateChangedForFaction",
                    "solarsystemid",
                    vec![
                        PyRep::Integer(crate::services::insurgency_services::CAMPAIGN_STATE_ACTIVE),
                        PyRep::Integer(p_fac),
                        PyRep::Integer(i64::from(fob_sys)),
                    ],
                );
            }
        }

        match action {
            Some("ResolveEmpireWin") => {
                let _ = evaluate_and_resolve_insurgency_campaign(ctx, cid, Some("EmpireWin"));
            }
            Some("Resolve") => {
                let _ = evaluate_and_resolve_insurgency_campaign(ctx, cid, None);
            }
            Some("End") => {
                let (end_info, unresolved_fob_win) = ctx.db.with_snapshot(|s| {
                    let info = s
                        .insurgency_runtime
                        .campaigns
                        .get(&cid)
                        .map(|c| (c.pirate_faction_id, c.systems.clone()));
                    let needs_resolve = !s.insurgency_runtime.campaign_outcomes.contains_key(&cid);
                    let fob_dest = s
                        .structures
                        .get(&(PIRATE_FOB_STRUCTURE_BASE_ID + cid))
                        .is_some_and(|st| st.state == "Destroyed");
                    (info, if needs_resolve { Some(fob_dest) } else { None })
                });
                if let Some(fob_dest) = unresolved_fob_win {
                    let _ = evaluate_and_resolve_insurgency_campaign(
                        ctx,
                        cid,
                        if fob_dest { Some("EmpireWin") } else { None },
                    );
                }
                end_insurgency_campaign(ctx, cid);
                if let Some((p_fac, ended_systems)) = end_info {
                    super::cleanup_ended_campaign_sites_and_roamers(ctx, p_fac, &ended_systems).await;
                }
            }
            _ => {}
        }
    }
}

#[path = "fw_gameplay_insurgency_lifecycle_cov_unit_tests.rs"]
#[cfg(test)]
mod fw_gameplay_insurgency_lifecycle_cov_unit_tests;

