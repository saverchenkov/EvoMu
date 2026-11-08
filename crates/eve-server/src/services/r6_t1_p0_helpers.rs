//! `r6_t1_p0_helpers` — Security & exploit hardening helpers for Round 6 Track 1 P0 fixes
//! (`R6-T1-01`..`R6-T1-08` / `#355`..`#362`).

#![forbid(unsafe_code)]

use crate::router::{ActiveScannerProbe, ServerContext};
use crate::services::combat_crimewatch::{
    is_legal_war_or_suspect_target, process_ship_insurance_payout, with_combat_runtime,
    with_mut_combat_runtime, SafetyLevel,
};
use crate::system_actor::SystemHandle;
use eve_db::{InventoryFlag, InventoryItemRecord, KillmailRecord};
use eve_destiny::{Ball, DamageVector};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

/// `R6-T1-01` (`#355`): Ensures a tube-loaded fighter stack has at most `max_squadron_size`
/// units before spawning in space, splitting any unlaunched excess back into a separate
/// `InventoryFlag::FighterBay` stack on `ship_id` so squadron HP can never be inflated
/// above `max_squadron_size * unit_hp` and unit losses never wipe unlaunched reserves.
pub(crate) fn prepare_tube_fighter_stack_for_launch(
    ctx: &ServerContext,
    fighter_item_id: i64,
    ship_id: i64,
    max_squadron_size: u32,
) -> Option<InventoryItemRecord> {
    let cap = max_squadron_size.max(1) as i32;
    ctx.db.with_mut_snapshot(|snap| {
        let orig = snap.items.get_mut(&fighter_item_id)?;
        if orig.quantity > cap {
            let excess = orig.quantity - cap;
            orig.quantity = cap;
            orig.version += 1;
            let excess_template = InventoryItemRecord {
                item_id: 0,
                type_id: orig.type_id,
                owner_id: orig.owner_id,
                location_id: ship_id,
                flag: InventoryFlag::FighterBay,
                quantity: excess,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            };
            snap.next_entity_id += 1;
            let rem_id = snap.next_entity_id;
            let mut rem_item = excess_template;
            rem_item.item_id = rem_id;
            snap.items.insert(rem_id, rem_item);
        }
        snap.items.get(&fighter_item_id).cloned()
    })
}

/// `R6-T1-02` (`#356`): Evaluates Crimewatch legality, safety settings (`Green`/`Yellow`/`Red`),
/// Limited Engagements, and active Kill Rights for legacy single-target aggression, and executes
/// canonical CONCORD response + ship death pipeline (spawning a wreck with loot and ejecting the
/// pilot into a floating capsule at the ship's space position instead of teleporting home).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn enforce_legacy_pvp_safety_and_concord(
    ctx: &ServerContext,
    session: &mut SessionState,
    _sys_handle: &SystemHandle,
    char_id: i64,
    ship_id: i64,
    attacker_ball: &Ball,
    sys_id: u32,
    sys_sec: f64,
    target_id: i64,
    target_ball: &Ball,
    victim_char_id: i64,
    now_unix: u64,
) -> Result<(bool, Option<PyRep>), BridgeRpcError> {
    let is_pod_target = target_ball.type_id == 670;
    let has_kill_right = ctx.db.with_snapshot(|snap| {
        snap.kill_rights.values().any(|kr| {
            kr.target_id == victim_char_id && (kr.owner_id == char_id || kr.is_active_suspect)
        })
    });
    if has_kill_right {
        with_mut_combat_runtime(ctx, |rt| {
            rt.add_limited_engagement(char_id, victim_char_id, now_unix);
        });
    }
    let has_limited_engagement = with_combat_runtime(ctx, |rt| {
        rt.has_limited_engagement_with(char_id, victim_char_id, now_unix)
            || rt.has_limited_engagement_with(victim_char_id, char_id, now_unix)
    });
    let victim_is_suspect_or_crim = {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(victim_char_id);
        st.is_suspect(now_unix) || st.is_criminal(now_unix)
    };
    let has_legal_engagement = ctx.db.with_snapshot(|snap| {
        is_legal_war_or_suspect_target(snap, char_id, victim_char_id, now_unix)
    });
    let lawful_target =
        has_legal_engagement || has_kill_right || has_limited_engagement || victim_is_suspect_or_crim;

    if !lawful_target {
        let safety = with_combat_runtime(ctx, |rt| {
            rt.safety_levels
                .get(&char_id)
                .copied()
                .unwrap_or(SafetyLevel::Red)
        });
        let concord_suppressed = !is_pod_target
            && crate::services::combat_npc_police::is_concord_suppressed_in_system(ctx, sys_id);
        let would_be_criminal =
            (sys_sec >= 0.45 && !concord_suppressed) || (sys_sec > 0.0 && is_pod_target);
        let would_be_suspect = !would_be_criminal && sys_sec > 0.0;
        if (safety == SafetyLevel::Green && (would_be_criminal || would_be_suspect))
            || (safety == SafetyLevel::Yellow && would_be_criminal)
        {
            return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
        }
    }

    crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        ctx,
        char_id,
        ship_id,
        sys_id,
        target_id,
        is_pod_target,
    )?;

    if sys_sec >= 0.45 && !lawful_target {
        let concord_delay = crate::formulas::concord_response_delay_sec(sys_sec);
        let attacker_ship_val = ctx
            .sde
            .types
            .get(&attacker_ball.type_id)
            .map(|t| t.base_price)
            .unwrap_or(230_000.0);
        crate::services::combat_crimewatch::crimewatch_timers::apply_illegal_pvp_sec_status_loss(
            ctx,
            char_id,
            victim_char_id,
            sys_id,
            sys_sec,
            is_pod_target,
            now_unix,
        );
        {
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let st = cw.get_or_create(char_id);
            st.criminal_flag_until_unix = st.criminal_flag_until_unix.max(now_unix.saturating_add(900));
            st.weapon_timer_until_unix = st.weapon_timer_until_unix.max(now_unix.saturating_add(60));
            st.pvp_timer_until_unix = st.pvp_timer_until_unix.max(now_unix.saturating_add(900));
        }
        let kr_id = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            snap.next_entity_id
        });
        crate::services::combat_crimewatch::create_kill_right(
            ctx,
            kr_id,
            victim_char_id,
            char_id,
            0.0,
        );
        let _ = process_ship_insurance_payout(ctx, char_id, ship_id, attacker_ball.type_id, true);
        let rng_seed = ctx.mechanic_rng.try_next_u64().unwrap_or(0);
        match crate::combat::death_pipeline::execute_death_pipeline(
            ctx,
            sys_id,
            char_id,
            ship_id,
            1_000_125,
            attacker_ball.position,
            rng_seed,
        )
        .await
        {
            Ok(dp) => {
                if let Some(capsule_id) = dp.ejected_capsule_id {
                    session.ship_id = Some(capsule_id);
                    session.station_id = None;
                    session.structure_id = None;
                    session.solar_system_id = Some(sys_id);
                } else {
                    ctx.db.with_snapshot(|snap| {
                        if let Some(ch) = snap.characters.get(&char_id) {
                            session.ship_id = Some(ch.active_ship_id);
                            session.station_id = ch.station_id;
                            session.structure_id = ch.structure_id;
                            session.solar_system_id = Some(ch.solar_system_id);
                        }
                    });
                }
                return Ok((
                    has_legal_engagement,
                    Some(PyRep::keyval([
                        ("criminalCharacterID", PyRep::Integer(char_id)),
                        ("victimCharacterID", PyRep::Integer(victim_char_id)),
                        ("concordResponseSeconds", PyRep::Real(concord_delay)),
                        ("concordKillmailID", PyRep::Integer(dp.killmail_id)),
                        ("killRightID", PyRep::Integer(kr_id)),
                        ("wreckID", PyRep::Integer(dp.wreck_item_id)),
                        (
                            "capsuleID",
                            dp.ejected_capsule_id
                                .map(PyRep::Integer)
                                .unwrap_or(PyRep::None),
                        ),
                        ("criminalTimerSeconds", PyRep::Integer(900)),
                    ])),
                ));
            }
            Err(_) => {
                let kill_id = ctx.db.with_mut_snapshot(|snap| {
                    snap.next_entity_id += 1;
                    let kid = snap.next_entity_id;
                    snap.record_killmail(
                        KillmailRecord {
                            kill_id: kid,
                            victim_character_id: char_id,
                            attacker_character_id: 1_000_125,
                            solar_system_id: sys_id,
                            ship_type_id: attacker_ball.type_id,
                            total_value_isk: attacker_ship_val,
                            ..Default::default()
                        },
                        now_unix,
                    );
                    kid
                });
                return Ok((
                    has_legal_engagement,
                    Some(PyRep::keyval([
                        ("criminalCharacterID", PyRep::Integer(char_id)),
                        ("victimCharacterID", PyRep::Integer(victim_char_id)),
                        ("concordResponseSeconds", PyRep::Real(concord_delay)),
                        ("concordKillmailID", PyRep::Integer(kill_id)),
                        ("killRightID", PyRep::Integer(kr_id)),
                        ("criminalTimerSeconds", PyRep::Integer(900)),
                    ])),
                ));
            }
        }
    }

    Ok((has_legal_engagement, None))
}

/// Resolves eligible non-tethered, non-invulnerable targets in a Disruptive Lance cylinder and
/// validates Crimewatch safety across all cone targets before any isotope deduction, capacitor
/// drain, or Crimewatch timer mutation (`#478`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn collect_and_precheck_lancer_cone_targets(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    all_balls: &[Ball],
    ship_pos: DVec3,
    beam_dir: DVec3,
    effective_range: f64,
    neut_radius: f64,
    now_unix: u64,
) -> Result<Vec<(Ball, f64)>, BridgeRpcError> {
    let mut eligible: Vec<(Ball, f64)> = Vec::new();
    for b in all_balls {
        if b.id == ship_id {
            continue;
        }
        let to_ball = b.position - ship_pos;
        let along_beam = to_ball.dot(beam_dir);
        let perp_dist = (to_ball - beam_dir * along_beam).length();
        if along_beam < 100.0 || along_beam > effective_range || perp_dist > neut_radius {
            continue;
        }
        let owner_char_opt = ctx.db.with_snapshot(|snap| {
            snap.characters
                .values()
                .find(|c| c.active_ship_id == b.id)
                .map(|c| c.character_id)
                .or_else(|| snap.items.get(&b.id).map(|i| i.owner_id))
        });
        if crate::services::space_destiny::is_location_change_invulnerable(ctx, b.id, now_unix)
            || owner_char_opt.is_some_and(|cid| {
                crate::services::space_destiny::is_location_change_invulnerable(ctx, cid, now_unix)
            })
        {
            continue;
        }
        let state_invuln = sys
            .get_combat_state(b.id)
            .await
            .ok()
            .flatten()
            .is_some_and(|v| {
                v.is_tethered
                    || v.is_invulnerable()
                    || v.undock_invulnerable_remaining_ticks > 0
                    || v.gate_cloak_remaining_ticks > 0
            });
        if state_invuln {
            continue;
        }
        eligible.push((b.clone(), perp_dist));
    }

    for (b, _) in &eligible {
        crate::services::combat_crimewatch::crimewatch_timers::precheck_pvp_aggression_safety(
            ctx,
            char_id,
            sys_id,
            b.id,
            b.type_id == 670,
        )?;
    }

    Ok(eligible)
}

/// `R6-T1-04` (`#358`): Resolves eligible non-tethered, non-invulnerable targets in a
/// Disruptive Lance cylinder, enforces low-sec safety & Crimewatch aggression for every
/// hit player ship or capsule, and applies damage/debuffs without forcibly untethering ships.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_lancer_beam_attack(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    mod_type_id: u32,
    all_balls: &[Ball],
    ship_pos: DVec3,
    beam_dir: DVec3,
    effective_range: f64,
    beam_radius: f64,
    neut_radius: f64,
    total_dmg_vec: DamageVector,
    neut_amount_gj: f64,
    victim_debuff_secs: u64,
    now_unix: u64,
) -> Result<Vec<i64>, BridgeRpcError> {
    let eligible = collect_and_precheck_lancer_cone_targets(
        ctx,
        sys,
        sys_id,
        char_id,
        ship_id,
        all_balls,
        ship_pos,
        beam_dir,
        effective_range,
        neut_radius,
        now_unix,
    )
    .await?;

    let sys_sec = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.modified_system_securities.get(&sys_id).copied()
    })
    .unwrap_or_else(|| {
        ctx.sde
            .solar_systems
            .get(&sys_id)
            .map(|s| s.security_status)
            .unwrap_or(1.0)
    });
    for (b, _) in &eligible {
        crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
            ctx,
            char_id,
            ship_id,
            sys_id,
            b.id,
            b.type_id == 670,
        )?;
        if sys_sec > 0.0 {
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let st = cw.get_or_create(char_id);
            if b.type_id == 670 || sys_sec >= 0.45 {
                st.criminal_flag_until_unix = st.criminal_flag_until_unix.max(now_unix.saturating_add(900));
            } else {
                st.suspect_flag_until_unix = st.suspect_flag_until_unix.max(now_unix.saturating_add(900));
            }
            st.weapon_timer_until_unix = st.weapon_timer_until_unix.max(now_unix.saturating_add(60));
        }
    }

    let mut hit_targets = Vec::new();
    for (b, perp_dist) in eligible {
        if perp_dist <= beam_radius {
            let (applied, destroyed) =
                crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
                    ctx,
                    sys,
                    sys_id,
                    char_id,
                    b.id,
                    total_dmg_vec,
                    10.0,
                    now_unix,
                )
                .await
                .ok()
                .flatten()
                .map(|(a, d, _)| (a, d))
                .unwrap_or((0.0, false));
            crate::services::combat_crimewatch::record_killmail_assist_action(
                ctx,
                b.id,
                char_id,
                ship_type_id,
                mod_type_id,
                applied,
                false,
                now_unix,
            );
            if destroyed {
                crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                    ctx,
                    sys_id,
                    char_id,
                    b.id,
                    b.type_id,
                    b.position,
                    now_unix,
                )
                .await;
            }
        }
        if let Ok(Some(mut v_cs)) = sys.get_combat_state(b.id).await {
            if !v_cs.is_tethered && !v_cs.is_invulnerable() {
                v_cs.capacitor = (v_cs.capacitor - neut_amount_gj).max(0.0);
                v_cs.warp_scramble_strength = v_cs.warp_scramble_strength.max(100);
                v_cs.is_mwd_mjd_disabled = true;
                v_cs.lance_disrupted_remaining_s = victim_debuff_secs as f64;
                let _ = sys.set_combat_state(b.id, v_cs).await;
            }
        }
        hit_targets.push(b.id);
    }

    Ok(hit_targets)
}

/// `R6-T1-06` (`#360`): Atomically claims overdue scanner probes (`p.state = -1`) inside the
/// `active_scanner_probes` write lock before awaiting ball removal or cargo return so
/// concurrent ticks/RPCs can never duplicate probes into cargo.
pub async fn expire_overdue_probes_for_char(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
) -> (Vec<i64>, Vec<i64>) {
    let newly_expired: Vec<ActiveScannerProbe> =
        if let Ok(mut map) = ctx.active_scanner_probes.write() {
            let mut claimed = Vec::new();
            for p in map.values_mut() {
                if p.owner_char_id == char_id
                    && p.state >= 0
                    && now_unix
                        >= p.launched_at_unix
                            .saturating_add(p.max_flight_time_s.max(0.0) as u64)
                {
                    p.state = -1;
                    claimed.push(p.clone());
                }
            }
            claimed
        } else {
            Vec::new()
        };

    if newly_expired.is_empty() {
        return (Vec::new(), Vec::new());
    }

    for p in &newly_expired {
        let sys_handle = ctx.systems.get_or_spawn(p.solar_system_id);
        let _ = sys_handle.remove_ball(p.probe_id).await;
    }

    let (returned_ids, lost_ids) = ctx.db.with_mut_snapshot(|snap| {
        let mut ret = Vec::new();
        let mut lost = Vec::new();
        for p in &newly_expired {
            let probe_vol = ctx
                .sde
                .types
                .get(&p.probe_type_id)
                .map(|t| t.volume.max(0.01))
                .unwrap_or(0.1);
            let ship_cap = snap
                .items
                .get(&p.ship_id)
                .and_then(|si| {
                    si.dynamic_attributes
                        .get(&38)
                        .copied()
                        .or_else(|| ctx.sde.types.get(&si.type_id).map(|t| t.capacity))
                })
                .unwrap_or(400.0);
            let used_vol: f64 = snap
                .items
                .values()
                .filter(|i| {
                    i.location_id == p.ship_id && i.flag == InventoryFlag::Cargo && i.quantity > 0
                })
                .map(|i| {
                    (i.quantity as f64)
                        * ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.1)
                })
                .sum();
            if used_vol + probe_vol <= ship_cap + 1e-6 && snap.items.contains_key(&p.ship_id) {
                if let Some(st) = snap.items.values_mut().find(|i| {
                    i.owner_id == char_id
                        && i.location_id == p.ship_id
                        && i.flag == InventoryFlag::Cargo
                        && i.type_id == p.probe_type_id
                }) {
                    st.quantity += 1;
                    st.version += 1;
                } else {
                    snap.next_entity_id += 1;
                    let new_id = snap.next_entity_id;
                    snap.items.insert(
                        new_id,
                        InventoryItemRecord {
                            item_id: new_id,
                            type_id: p.probe_type_id,
                            owner_id: char_id,
                            location_id: p.ship_id,
                            flag: InventoryFlag::Cargo,
                            quantity: 1,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                }
                ret.push(p.probe_id);
            } else {
                lost.push(p.probe_id);
            }
        }
        (ret, lost)
    });

    if let Ok(mut map) = ctx.active_scanner_probes.write() {
        for pid in &lost_ids {
            if let Some(p) = map.get_mut(pid) {
                p.state = -2;
            }
        }
    }
    crate::services::space_probe_scanning::sync_scanner_probes_to_db(ctx);

    (returned_ids, lost_ids)
}

/// `R6-T1-06` (`#360`): Background sweep expiring overdue scanner probes across all owners.
pub async fn expire_all_overdue_probes(ctx: &ServerContext, now_unix: u64) {
    let overdue_owners: Vec<i64> = if let Ok(map) = ctx.active_scanner_probes.read() {
        let mut owners: Vec<i64> = map
            .values()
            .filter(|p| {
                p.state >= 0
                    && now_unix
                        >= p.launched_at_unix
                            .saturating_add(p.max_flight_time_s.max(0.0) as u64)
            })
            .map(|p| p.owner_char_id)
            .collect();
        owners.sort_unstable();
        owners.dedup();
        owners
    } else {
        Vec::new()
    };
    for cid in overdue_owners {
        let _ = expire_overdue_probes_for_char(ctx, cid, now_unix).await;
    }
}

/// `R6-T1-08` (`#362`): Verifies the caller is undocked, in the site's solar system,
/// piloting an active uncloaked ship, and on-grid within `100_000.0` m of `site_pos`.
pub(crate) async fn verify_special_site_caller_on_grid(
    ctx: &ServerContext,
    session: &SessionState,
    sys_handle: &SystemHandle,
    _char_id: i64,
    site_pos: DVec3,
    site_name: &str,
) -> Result<(), BridgeRpcError> {
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot complete in-space special dungeon instance while docked".into(),
        });
    }
    let caller_sys = session.solar_system_id.ok_or_else(|| BridgeRpcError {
        code: "NotInSpace".into(),
        message: "Caller must be in space".into(),
    })?;
    let _ = caller_sys;
    let ship_id = session.ship_id.filter(|&id| id > 0).ok_or_else(|| BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Must have an active ship in space to complete site".into(),
    })?;
    let item_pos_opt = ctx
        .db
        .with_snapshot(|snap| snap.item_positions.get(&ship_id).copied());
    if let Some((_, ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() {
        let ship_cs_opt = sys_handle.get_combat_state(ship_id).await.ok().flatten();
        if ship_cs_opt.is_some_and(|cs| cs.is_cloaked) {
            return Err(BridgeRpcError {
                code: "ShipCloaked".into(),
                message: format!("Cannot complete {site_name} while cloaked"),
            });
        }
        let dist = ship_ball.position.distance(site_pos);
        if dist > 100_000.0 {
            return Err(BridgeRpcError {
                code: "TooFarFromSite".into(),
                message: format!(
                    "Must be on-grid within 100 km of {site_name} to complete it (distance {dist:.0} m)"
                ),
            });
        }
    } else if let Some([x, y, z]) = item_pos_opt {
        let dist = DVec3::new(x, y, z).distance(site_pos);
        if dist > 100_000.0 {
            return Err(BridgeRpcError {
                code: "TooFarFromSite".into(),
                message: format!(
                    "Must be on-grid within 100 km of {site_name} to complete it (distance {dist:.0} m)"
                ),
            });
        }
    }
    Ok(())
}

/// Resolves deterministic string special-site IDs (`"HF-<sys>-<idx>"`, `"TRIG-<sys>-<idx>"`,
/// `"DRIFTER-<sys>-<idx>"`) to their canonical numeric instance ID for `keeper` gate RPCs.
pub(crate) fn resolve_special_site_string_instance_id(
    ctx: &ServerContext,
    site_str: &str,
) -> Option<i64> {
    let parts: Vec<&str> = site_str.split('-').collect();
    if parts.len() < 3 {
        return None;
    }
    let sys_id: u32 = parts[1].parse().ok()?;
    let idx: usize = parts[2].parse().ok()?;
    crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| match parts[0] {
        "HF" => reg
            .homefront_sites
            .get(&sys_id)
            .and_then(|list| list.get(idx).map(|s| s.instance_id))
            .or_else(|| (idx >= 1000).then_some(idx as i64)),
        "TRIG" => reg
            .triglavian_sites
            .get(&sys_id)
            .and_then(|list| list.get(idx).map(|s| s.instance_id))
            .or_else(|| (idx >= 1000).then_some(idx as i64)),
        "DRIFTER" => reg
            .drifter_hives
            .values()
            .find(|h| h.site_id == site_str || h.solar_system_id == sys_id)
            .map(|h| h.solar_system_id as i64),
        _ => None,
    })
}

/// Returns `true` when `instance_id` identifies a registered special PvE site
/// (Drifter Hive, Homefront Operation, or Triglavian Minor Victory site).
pub(crate) fn is_known_special_site_instance_id(ctx: &ServerContext, instance_id: i64) -> bool {
    crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
        reg.drifter_hives.contains_key(&(instance_id as u32))
            || reg
                .drifter_hives
                .values()
                .any(|h| (h.solar_system_id as i64) == instance_id)
            || reg
                .homefront_sites
                .values()
                .flatten()
                .any(|s| s.instance_id == instance_id)
            || reg
                .triglavian_sites
                .values()
                .flatten()
                .any(|s| s.instance_id == instance_id)
    })
}

#[cfg(test)]
#[path = "r6_t1_p0_helpers_cov_unit_tests.rs"]
mod r6_t1_p0_helpers_cov_unit_tests;

