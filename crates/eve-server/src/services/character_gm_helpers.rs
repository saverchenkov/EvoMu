//! Shared GM slash parsing, target resolution, `/heal`, and spawn rollback helpers (`character_gm_helpers`).

#![forbid(unsafe_code)]

use crate::router::notify_registry::OnRemoteMessage;
use crate::router::{NotificationTarget, ServerContext};
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

/// Executes a pre-parsed `/heal` command via [`crate::domain::combat_ops::restore_ship_vital_state`].
pub(crate) async fn execute_gm_heal_parsed(
    ctx: &ServerContext,
    session: &SessionState,
    cid: i64,
    cmd: &str,
    args: crate::router::slash_registry::HealSlashArgs,
) -> Result<PyRep, BridgeRpcError> {
    let arg_id = match args.target {
        crate::router::slash_registry::HealSlashTarget::SelfShip => None,
        crate::router::slash_registry::HealSlashTarget::EntityOrCharId(id) => Some(id),
    };
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

    let healed = crate::domain::combat_ops::restore_ship_vital_state(
        ctx,
        crate::domain::combat_ops::RestoreShipVitalsRequest {
            target_char_id,
            target_ship_id,
            ship_type_id,
            sys_id,
        },
    )
    .await
    .map_err(crate::services::domain_err_to_rpc)?;

    if healed.character_id > 0 && healed.ship_id > 0 {
        let _ = ctx.emit_typed(
            NotificationTarget::Character(healed.character_id),
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
        ("characterID", PyRep::Integer(healed.character_id)),
        ("shipID", PyRep::Integer(healed.ship_id)),
        ("targetID", PyRep::Integer(healed.ship_id)),
        ("shieldHP", PyRep::Real(healed.shield_hp)),
        ("armorHP", PyRep::Real(healed.armor_hp)),
        ("hullHP", PyRep::Real(healed.hull_hp)),
        ("capacitor", PyRep::Real(healed.capacitor)),
        ("moduleHeatCleared", PyRep::Bool(true)),
        ("healed", PyRep::Bool(true)),
    ]))
}

/// Parses and executes `/heal [target_id]` for unit tests (`#1193`).
#[cfg(test)]
pub(crate) async fn execute_gm_heal(
    ctx: &ServerContext,
    session: &SessionState,
    cid: i64,
    cmd: &str,
) -> Result<PyRep, BridgeRpcError> {
    let rest = crate::router::slash_registry::parse_slash_invocation(cmd)
        .map(|inv| inv.rest)
        .unwrap_or_else(|| cmd.trim());
    let parsed = crate::router::slash_registry::parse_heal_slash_args(rest)?;
    execute_gm_heal_parsed(ctx, session, cid, cmd, parsed).await
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
    use eve_destiny::CombatState;

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
