//! Shared GM slash parsing, target resolution, `/heal`, and spawn rollback helpers (`character_gm_helpers`).

#![forbid(unsafe_code)]

use crate::router::notify_registry::{OnModuleAttributeChanges, OnRemoteMessage};
use crate::router::{NotificationTarget, ServerContext};
use eve_destiny::CombatState;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub(crate) fn parse_slash_tokens(rest: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut in_token = false;
    for ch in rest.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                cur.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
            in_token = true;
        } else if ch.is_whitespace() {
            if in_token {
                tokens.push(std::mem::take(&mut cur));
                in_token = false;
            }
        } else {
            cur.push(ch);
            in_token = true;
        }
    }
    if in_token {
        tokens.push(cur);
    }
    tokens
}

pub(crate) fn is_me_or_myship(tok: &str) -> bool {
    tok.eq_ignore_ascii_case("me") || tok.eq_ignore_ascii_case("myship")
}

pub(crate) fn resolve_item_or_char_active_ship(
    snap: &eve_db::WorldStateSnapshot,
    cid: i64,
    item_tok: &str,
) -> Option<i64> {
    if is_me_or_myship(item_tok) || item_tok.eq_ignore_ascii_case("self") {
        return snap.characters.get(&cid).map(|c| c.active_ship_id);
    }
    let parsed = item_tok.parse::<i64>().ok()?;
    if snap.items.contains_key(&parsed) {
        Some(parsed)
    } else if let Some(ch) = snap.characters.get(&parsed) {
        Some(ch.active_ship_id)
    } else {
        Some(parsed)
    }
}

pub(crate) fn resolve_gm_char_prefix<'a>(
    ctx: &ServerContext,
    cid: i64,
    tokens: &'a [String],
    require_min_remaining: usize,
) -> (i64, &'a [String]) {
    if tokens.len() <= require_min_remaining {
        return (cid, tokens);
    }
    let first = &tokens[0];
    if is_me_or_myship(first) || first.eq_ignore_ascii_case("self") {
        return (cid, &tokens[1..]);
    }
    if let Ok(parsed) = first.parse::<i64>() {
        if parsed > 0
            && (parsed == cid || ctx.db.with_snapshot(|s| s.characters.contains_key(&parsed)))
        {
            return (parsed, &tokens[1..]);
        }
    }
    (cid, tokens)
}

/// Executes `/heal [target_id]` across the live ballpark `CombatState`, persisted SP6/DB HP &
/// capacitor dynamic attributes, fitting heat/damage state, Destiny `DoDestinyUpdate`
/// `OnDamageStateChange`, and `godma` `OnModuleAttributeChanges`.
pub(crate) async fn execute_gm_heal(
    ctx: &ServerContext,
    session: &SessionState,
    cid: i64,
    cmd: &str,
) -> Result<PyRep, BridgeRpcError> {
    let tokens = parse_slash_tokens(cmd);
    let arg_tokens = match tokens.first().map(String::as_str) {
        Some(head) if head.eq_ignore_ascii_case("/heal") => &tokens[1..],
        _ => &tokens[..],
    };
    let arg_id = arg_tokens
        .first()
        .filter(|s| !is_me_or_myship(s))
        .and_then(|s| s.parse::<i64>().ok());

    let (target_char_id, sys_id, target_ship_id, ship_type_id) = ctx.db.with_snapshot(|snap| {
        let db_char = snap.characters.get(&cid);
        let caller_sys = session
            .solar_system_id
            .filter(|&s| s > 0)
            .or_else(|| db_char.map(|c| c.solar_system_id).filter(|&s| s > 0))
            .unwrap_or(0);
        let caller_ship = session
            .ship_id
            .filter(|&id| id > 0)
            .or_else(|| db_char.map(|c| c.active_ship_id).filter(|&id| id > 0))
            .unwrap_or(0);
        if let Some(tid) = arg_id {
            if let Some(ch) = snap.characters.get(&tid) {
                let sid = ch.active_ship_id;
                let stid = snap.items.get(&sid).map(|i| i.type_id).unwrap_or(0);
                (tid, ch.solar_system_id, sid, stid)
            } else if let Some(item) = snap.items.get(&tid) {
                let owner_sys = snap
                    .characters
                    .get(&item.owner_id)
                    .map(|c| c.solar_system_id)
                    .unwrap_or(caller_sys);
                (item.owner_id, owner_sys, tid, item.type_id)
            } else {
                (cid, caller_sys, tid, 0)
            }
        } else {
            let stid = snap.items.get(&caller_ship).map(|i| i.type_id).unwrap_or(0);
            (cid, caller_sys, caller_ship, stid)
        }
    });

    let fit_opt = if target_ship_id > 0 && target_char_id > 0 {
        ctx.fit(target_char_id, target_ship_id).ok()
    } else {
        None
    };
    let (
        fit_max_hull,
        fit_max_armor,
        fit_max_shield,
        fit_max_cap,
        fit_cap_recharge_ms,
        fit_shield_recharge_ms,
    ) = if let Some(ref fit) = fit_opt {
        let mut eff_attrs: std::collections::HashMap<u16, f64> = [
            eve_sde::attr_ids::HP_STRUCT,
            eve_sde::attr_ids::ARMOR_HP,
            eve_sde::attr_ids::SHIELD_CAPACITY,
            eve_sde::attr_ids::CAPACITOR_CAPACITY,
            eve_sde::attr_ids::RECHARGE_RATE,
            eve_sde::attr_ids::SHIELD_RECHARGE_RATE,
        ]
        .into_iter()
        .map(|aid| (aid, fit.ship.get_attribute(aid, &ctx.sde)))
        .collect();
        crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(
            ctx,
            target_ship_id,
            &mut eff_attrs,
        );
        (
            eff_attrs
                .get(&eve_sde::attr_ids::HP_STRUCT)
                .copied()
                .unwrap_or(0.0),
            eff_attrs
                .get(&eve_sde::attr_ids::ARMOR_HP)
                .copied()
                .unwrap_or(0.0),
            eff_attrs
                .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                .copied()
                .unwrap_or(0.0),
            eff_attrs
                .get(&eve_sde::attr_ids::CAPACITOR_CAPACITY)
                .copied()
                .unwrap_or(0.0),
            eff_attrs
                .get(&eve_sde::attr_ids::RECHARGE_RATE)
                .copied()
                .unwrap_or(0.0),
            eff_attrs
                .get(&eve_sde::attr_ids::SHIELD_RECHARGE_RATE)
                .copied()
                .unwrap_or(0.0),
        )
    } else if let Some(t) = ctx.sde.types.get(&ship_type_id) {
        (
            t.default_attributes
                .get(&eve_sde::attr_ids::HP_STRUCT)
                .copied()
                .unwrap_or(0.0),
            t.default_attributes
                .get(&eve_sde::attr_ids::ARMOR_HP)
                .copied()
                .unwrap_or(0.0),
            t.default_attributes
                .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                .copied()
                .unwrap_or(0.0),
            t.default_attributes
                .get(&eve_sde::attr_ids::CAPACITOR_CAPACITY)
                .copied()
                .unwrap_or(0.0),
            t.default_attributes
                .get(&eve_sde::attr_ids::RECHARGE_RATE)
                .copied()
                .unwrap_or(0.0),
            t.default_attributes
                .get(&eve_sde::attr_ids::SHIELD_RECHARGE_RATE)
                .copied()
                .unwrap_or(0.0),
        )
    } else {
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
    };

    let handle_opt = if target_ship_id > 0 {
        Some(ctx.systems.get_or_spawn(sys_id))
    } else {
        None
    };
    let pre_cs = if let Some(ref handle) = handle_opt {
        handle
            .get_combat_state(target_ship_id)
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    let unified_fallback_maxes = if let Some(ref cs) = pre_cs {
        (
            if cs.max_structure_hp > 0.0 {
                cs.max_structure_hp
            } else {
                fit_max_hull
            },
            if cs.max_armor_hp > 0.0 {
                cs.max_armor_hp
            } else {
                fit_max_armor
            },
            if cs.max_shield_hp > 0.0 {
                cs.max_shield_hp
            } else {
                fit_max_shield
            },
        )
    } else {
        (fit_max_hull, fit_max_armor, fit_max_shield)
    };
    let (pre_unified, pre_dyn_shield_charge, pre_dyn_cap, mut repaired_child_attrs, pre_ship_heat) =
        ctx.db.with_snapshot(|snap| {
            let unified = if target_ship_id > 0 {
                Some(crate::sp6_lifecycle::read_unified_ship_damage_state(
                    ctx,
                    snap,
                    target_ship_id,
                    ship_type_id,
                    Some(unified_fallback_maxes),
                ))
            } else {
                None
            };
            let ship_it = snap.items.get(&target_ship_id);
            let dyn_shield = ship_it.and_then(|it| {
                it.dynamic_attributes
                    .get(&eve_sde::attr_ids::SHIELD_CHARGE)
                    .copied()
            });
            let dyn_cap = ship_it.and_then(|it| it.dynamic_attributes.get(&18u16).copied());
            let ship_heat = ship_it
                .and_then(|it| it.dynamic_attributes.get(&1211u16).copied())
                .unwrap_or(0.0);
            let mut child_changes: Vec<(i64, u16, f64)> = Vec::new();
            if target_ship_id > 0 {
                for item in snap.items.values() {
                    if item.location_id == target_ship_id {
                        if let Some(&old_d) = item.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)
                        {
                            if old_d > 0.0 {
                                child_changes.push((
                                    item.item_id,
                                    eve_sde::attr_ids::DAMAGE,
                                    old_d,
                                ));
                            }
                        }
                        if let Some(&old_h) = item.dynamic_attributes.get(&1211u16) {
                            if old_h > 0.0 {
                                child_changes.push((item.item_id, 1211u16, old_h));
                            }
                        }
                    }
                }
            }
            (unified, dyn_shield, dyn_cap, child_changes, ship_heat)
        });

    let mut combat_healed = CombatState::default();
    if let Some(ref handle) = handle_opt {
        if let Some(ref cs) = pre_cs {
            let needs_cap_sync = fit_max_cap > 0.0 && cs.max_capacitor + 1e-6 < fit_max_cap;
            let needs_recharge_sync = (fit_cap_recharge_ms > 0.0
                && (cs.capacitor_recharge_time_ms <= 0.0
                    || (needs_cap_sync
                        && (cs.capacitor_recharge_time_ms - fit_cap_recharge_ms).abs() > 1e-3)))
                || (fit_shield_recharge_ms > 0.0 && cs.shield_recharge_time_ms <= 0.0);
            let needs_zero_hp_sync = (cs.max_shield_hp <= 0.0 && fit_max_shield > 0.0)
                || (cs.max_armor_hp <= 0.0 && fit_max_armor > 0.0)
                || (cs.max_structure_hp <= 0.0 && fit_max_hull > 0.0);
            if needs_cap_sync || needs_recharge_sync || needs_zero_hp_sync {
                let _ = handle
                    .modify_combat_state(target_ship_id, move |st| {
                        if needs_cap_sync {
                            st.max_capacitor = fit_max_cap;
                            st.base_max_capacitor = Some(fit_max_cap);
                        }
                        if fit_cap_recharge_ms > 0.0
                            && (st.capacitor_recharge_time_ms <= 0.0 || needs_cap_sync)
                        {
                            st.capacitor_recharge_time_ms = fit_cap_recharge_ms;
                            st.base_capacitor_recharge_time_ms = Some(fit_cap_recharge_ms);
                        }
                        if fit_shield_recharge_ms > 0.0 && st.shield_recharge_time_ms <= 0.0 {
                            st.shield_recharge_time_ms = fit_shield_recharge_ms;
                        }
                        if st.max_shield_hp <= 0.0 && fit_max_shield > 0.0 {
                            st.max_shield_hp = fit_max_shield;
                            st.base_max_shield_hp = Some(fit_max_shield);
                        }
                        if st.max_armor_hp <= 0.0 && fit_max_armor > 0.0 {
                            st.max_armor_hp = fit_max_armor;
                            st.base_max_armor_hp = Some(fit_max_armor);
                        }
                        if st.max_structure_hp <= 0.0 && fit_max_hull > 0.0 {
                            st.max_structure_hp = fit_max_hull;
                            st.base_max_structure_hp = Some(fit_max_hull);
                        }
                    })
                    .await;
            }
        }
        if let Ok(cs) = handle.heal_combat_state(target_ship_id).await {
            combat_healed = cs;
        }
        let _ = handle.flush_peer_observer_updates(None).await;
    }

    if target_ship_id > 0 {
        ctx.db.with_mut_snapshot(|snap| {
            crate::sp6_lifecycle::clear_ship_combat_damage_in_snap(snap, target_ship_id);
            for item in snap.items.values_mut() {
                if item.item_id == target_ship_id || item.location_id == target_ship_id {
                    let r1 = item
                        .dynamic_attributes
                        .remove(&eve_sde::attr_ids::DAMAGE)
                        .is_some();
                    let r2 = item
                        .dynamic_attributes
                        .remove(&eve_sde::attr_ids::ARMOR_DAMAGE)
                        .is_some();
                    let r3 = item.dynamic_attributes.remove(&1211u16).is_some();
                    if (r1 || r2 || r3) && item.item_id != target_ship_id {
                        item.version += 1;
                    }
                }
            }
        });
    }
    let pre_rack_heat = fit_opt
        .as_ref()
        .map(|f| f.rack_heat)
        .unwrap_or([0.0, 0.0, 0.0]);
    if let Some(ref fit) = fit_opt {
        for m in fit.modules.values() {
            if m.damage_hp > 0.0
                && !repaired_child_attrs
                    .iter()
                    .any(|&(id, aid, _)| id == m.item_id && aid == eve_sde::attr_ids::DAMAGE)
            {
                repaired_child_attrs.push((m.item_id, eve_sde::attr_ids::DAMAGE, m.damage_hp));
            }
        }
        ctx.update_ship_rack_heat(target_char_id, target_ship_id, [0.0, 0.0, 0.0], true);
    }

    let eff_max_shield = combat_healed.max_shield_hp.max(fit_max_shield);
    let eff_max_armor = combat_healed.max_armor_hp.max(fit_max_armor);
    let eff_max_hull = combat_healed.max_structure_hp.max(fit_max_hull);
    let eff_max_cap = combat_healed.max_capacitor.max(fit_max_cap);

    let final_shield = if combat_healed.max_shield_hp > 0.0 {
        combat_healed.shield_hp
    } else {
        eff_max_shield
    };
    let final_armor = if combat_healed.max_armor_hp > 0.0 {
        combat_healed.armor_hp
    } else {
        eff_max_armor
    };
    let final_hull = if combat_healed.max_structure_hp > 0.0 {
        combat_healed.structure_hp
    } else {
        eff_max_hull
    };
    let final_cap = if combat_healed.max_capacitor > 0.0 {
        combat_healed.capacitor
    } else {
        eff_max_cap
    };

    if target_char_id > 0 && target_ship_id > 0 {
        let now_ft = ctx.now_system_sim_filetime_ticks_for_ship(target_char_id, target_ship_id);
        let mut attr_changes: Vec<PyRep> = Vec::new();
        let push_change =
            |list: &mut Vec<PyRep>, item_id: i64, attr_id: u16, new_val: f64, old_val: f64| {
                list.push(PyRep::Tuple(vec![
                    PyRep::String("OnModuleAttributeChange".into()),
                    PyRep::Integer(target_char_id),
                    PyRep::Integer(item_id),
                    PyRep::Integer(i64::from(attr_id)),
                    PyRep::Integer(now_ft),
                    PyRep::Real(new_val),
                    PyRep::Real(old_val),
                    PyRep::None,
                ]));
            };

        let (old_shield, old_armor_dmg, old_hull_dmg) = if let Some(u) = pre_unified {
            let s = match pre_dyn_shield_charge {
                Some(dc) if pre_cs.as_ref().is_none_or(|c| c.max_shield_hp <= 0.0) => {
                    dc.clamp(0.0, final_shield.max(dc))
                }
                Some(dc) => u.shield_hp.min(dc).max(0.0),
                None => u.shield_hp,
            };
            (s, u.armor_damage, u.hull_damage)
        } else {
            (pre_dyn_shield_charge.unwrap_or(final_shield), 0.0, 0.0)
        };

        let old_cap = match (pre_cs.as_ref().filter(|c| c.max_capacitor > 0.0), pre_dyn_cap) {
            (Some(cs), Some(dc)) => cs.capacitor.min(dc).max(0.0),
            (Some(cs), None) => cs.capacitor.max(0.0),
            (None, Some(dc)) => dc.max(0.0),
            (None, None) => final_cap,
        };

        if final_shield > 0.0 || pre_dyn_shield_charge.is_some() {
            push_change(
                &mut attr_changes,
                target_ship_id,
                eve_sde::attr_ids::SHIELD_CHARGE,
                final_shield,
                old_shield,
            );
        }
        if final_armor > 0.0 || old_armor_dmg > 0.0 {
            push_change(
                &mut attr_changes,
                target_ship_id,
                eve_sde::attr_ids::ARMOR_DAMAGE,
                0.0,
                old_armor_dmg,
            );
        }
        if final_hull > 0.0 || old_hull_dmg > 0.0 {
            push_change(
                &mut attr_changes,
                target_ship_id,
                eve_sde::attr_ids::DAMAGE,
                0.0,
                old_hull_dmg,
            );
        }
        let emit_cap_sync = (final_cap > 0.0 || pre_dyn_cap.is_some()) && eff_max_cap > 0.0;
        if !emit_cap_sync && (final_cap > 0.0 || pre_dyn_cap.is_some()) {
            push_change(
                &mut attr_changes,
                target_ship_id,
                18u16,
                final_cap,
                old_cap,
            );
        }
        if pre_ship_heat > 0.0 {
            push_change(&mut attr_changes, target_ship_id, 1211, 0.0, pre_ship_heat);
        }
        for (idx, rack_attr) in [1175u16, 1176, 1177].into_iter().enumerate() {
            if pre_rack_heat[idx] > 0.0 {
                push_change(
                    &mut attr_changes,
                    target_ship_id,
                    rack_attr,
                    0.0,
                    pre_rack_heat[idx],
                );
            }
        }
        repaired_child_attrs.sort_unstable_by_key(|&(id, aid, _)| (id, aid));
        for (mod_id, aid, old_val) in repaired_child_attrs {
            push_change(&mut attr_changes, mod_id, aid, 0.0, old_val);
        }
        if !attr_changes.is_empty() {
            ctx.emit_typed(
                NotificationTarget::Character(target_char_id),
                OnModuleAttributeChanges {
                    changes: PyRep::List(attr_changes),
                },
            );
        }
        if emit_cap_sync {
            let _ = ctx.notify_ship_capacitor_change(
                target_char_id,
                target_ship_id,
                final_cap,
                eff_max_cap,
                Some(old_cap),
            );
        }
        let _ = ctx.notify_damage_state_change(
            NotificationTarget::Character(target_char_id),
            target_ship_id,
            1.0,
            1.0,
            1.0,
        );
        let _ = ctx.emit_typed(
            NotificationTarget::Character(target_char_id),
            OnRemoteMessage {
                msg_id: "CustomNotify".into(),
                dict: Some(PyRep::Dict(vec![(
                    PyRep::String("notify".into()),
                    PyRep::String("Ship healed".into()),
                )])),
                kw: None,
            },
        );
    }

    Ok(PyRep::keyval([
        ("command", PyRep::String(cmd.to_string())),
        ("characterID", PyRep::Integer(target_char_id)),
        ("shipID", PyRep::Integer(target_ship_id)),
        ("targetID", PyRep::Integer(target_ship_id)),
        ("shieldHP", PyRep::Real(final_shield)),
        ("armorHP", PyRep::Real(final_armor)),
        ("hullHP", PyRep::Real(final_hull)),
        ("capacitor", PyRep::Real(final_cap)),
        ("moduleHeatCleared", PyRep::Bool(true)),
        ("healed", PyRep::Bool(true)),
    ]))
}

/// Clears runtime NPC AI and combat state for `entity_id` in `sys_id`.
pub(crate) fn remove_npc_runtime_state(ctx: &ServerContext, sys_id: u32, entity_id: i64) {
    if let Ok(mut map) = ctx.npc_spawns.write() {
        if let Some(mgr) = map.get_mut(&sys_id) {
            mgr.entities.remove(&entity_id);
        }
    }
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.npc_entities.remove(&entity_id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn execute_gm_heal_prefers_session_location_and_handles_leading_whitespace() {
        let ctx = ServerContext::new();
        let char_id = 90_000_001_i64;
        let stale_ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
        let ship_tid = ctx.db.with_snapshot(|s| s.items[&stale_ship_id].type_id);
        let session_sys_id = 30_000_142_u32;
        let session_ship_id = 980_949_777_i64;

        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.solar_system_id = 0;
            ch.active_ship_id = stale_ship_id;
            let mut sh = eve_db::InventoryItemRecord {
                item_id: session_ship_id,
                type_id: ship_tid,
                owner_id: char_id,
                location_id: i64::from(session_sys_id),
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            };
            sh.dynamic_attributes
                .insert(eve_sde::attr_ids::DAMAGE, 75.0);
            snap.items.insert(session_ship_id, sh);
            if let Some(stale) = snap.items.get_mut(&stale_ship_id) {
                stale
                    .dynamic_attributes
                    .insert(eve_sde::attr_ids::DAMAGE, 50.0);
            }
        });

        let handle = ctx.systems.get_or_spawn(session_sys_id);
        handle
            .spawn_ball(eve_destiny::Ball {
                id: session_ship_id,
                type_id: ship_tid,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.5,
                mass: 1_200_000.0,
                radius: 45.0,
                speed_fraction: 1.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await
            .unwrap();
        let damaged_cs = CombatState {
            shield_hp: 120.0,
            max_shield_hp: 500.0,
            armor_hp: 80.0,
            max_armor_hp: 400.0,
            structure_hp: 100.0,
            max_structure_hp: 300.0,
            capacitor: 45.0,
            max_capacitor: 250.0,
            ..Default::default()
        };
        handle
            .set_combat_state(session_ship_id, damaged_cs)
            .await
            .unwrap();

        let sess = SessionState {
            session_id: 77,
            user_id: Some(1),
            character_id: Some(char_id),
            solar_system_id: Some(session_sys_id),
            ship_id: Some(session_ship_id),
            ..SessionState::new(77)
        };

        let res = execute_gm_heal(&ctx, &sess, char_id, "  /heal  ")
            .await
            .expect("leading-whitespace /heal succeeds");
        assert_eq!(res.dict_get("shipID"), Some(&PyRep::Integer(session_ship_id)));

        let healed_hull = handle
            .get_combat_state(session_ship_id)
            .await
            .unwrap()
            .map(|s| s.structure_hp);
        assert_eq!(healed_hull, Some(300.0));

        ctx.db.with_snapshot(|snap| {
            assert!(
                !snap.items[&session_ship_id]
                    .dynamic_attributes
                    .contains_key(&eve_sde::attr_ids::DAMAGE),
                "session ship damage must be cleared"
            );
            assert_eq!(
                snap.items[&stale_ship_id]
                    .dynamic_attributes
                    .get(&eve_sde::attr_ids::DAMAGE),
                Some(&50.0),
                "stale DB ship must not be targeted when session.ship_id is valid"
            );
        });

        let explicit_res = execute_gm_heal(
            &ctx,
            &sess,
            char_id,
            &format!("  /heal   {stale_ship_id}"),
        )
        .await
        .expect("leading-whitespace /heal with explicit target succeeds");
        assert_eq!(
            explicit_res.dict_get("shipID"),
            Some(&PyRep::Integer(stale_ship_id))
        );
        ctx.db.with_snapshot(|snap| {
            assert!(
                !snap.items[&stale_ship_id]
                    .dynamic_attributes
                    .contains_key(&eve_sde::attr_ids::DAMAGE),
                "explicit target ship must be healed when leading whitespace precedes /heal"
            );
        });
    }
}
