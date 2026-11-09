//! `invbroker.LootAll` and `warStatisticMgr.GetKillMail` handlers extracted from
//! `combat_wrecks_killmails.rs` (`#484`, `#486`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use crate::services::combat_crimewatch::{with_combat_runtime, SafetyLevel};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub(super) async fn handle_loot_all(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "CantDoThatWhileDocked".into(),
            message: "Cannot loot space wrecks or containers while docked".into(),
        });
    }
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    let (ship_docked_in_db, entry_db_state) = ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&char_id);
        let ch_st = ch.and_then(|c| c.station_id.or(c.structure_id));
        let docked = ch_st.is_some_and(|st_id| {
            s.items
                .get(&ship_id)
                .is_some_and(|si| si.location_id == st_id)
        });
        let db_st = ch.map(|c| (c.station_id, c.structure_id, c.solar_system_id, c.active_ship_id));
        (docked, db_st)
    });
    if ship_docked_in_db {
        return Err(BridgeRpcError {
            code: "CantDoThatWhileDocked".into(),
            message: "Cannot loot space wrecks or containers while docked".into(),
        });
    }
    let Some(wreck_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "LootAll requires a container/wreck itemID".into(),
        });
    };
    let now_unix = ctx.now_unix();
    let Some(sys_id) = session.solar_system_id.or_else(|| {
        ctx.db
            .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
    }) else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Character has no active solar system".into(),
        });
    };
    let _ = super::step_wreck_and_jetcan_decay(ctx, sys_id, now_unix).await;

    let wreck_opt = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&wreck_id).cloned());
    let Some(wreck_item) = wreck_opt else {
        return Err(BridgeRpcError {
            code: "ContainerNotFound".into(),
            message: format!("Wreck or container {wreck_id} no longer exists in space"),
        });
    };
    if wreck_item.location_id != sys_id as i64 {
        return Err(BridgeRpcError {
            code: "InvalidContainerLocation".into(),
            message: format!("Container {wreck_id} is not in solar system {sys_id}"),
        });
    }
    let (is_ship_or_station, is_foreign_deployable, is_locked_container) =
        ctx.db.with_snapshot(|snap| {
            let Some(t) = ctx.sde.types.get(&wreck_item.type_id) else {
                return (false, false, false);
            };
            let ship_or_st = matches!(t.category_id, 3 | 6 | 65);
            let foreign_deployable =
                matches!(t.group_id, 1246 | 1250) && wreck_item.owner_id != char_id;
            let locked_cont = if matches!(t.group_id, 340 | 448) && wreck_item.owner_id != char_id {
                let st = crate::services::inventory_fitting::fitting_containers_ships::read_container_ship_state_snap(snap);
                st.container_passwords
                    .get(&wreck_id)
                    .is_some_and(|pw| !pw.is_empty())
                    && !st.unlocked_containers.contains(&(wreck_id, char_id))
                    && !st.unlocked_containers.contains(&(char_id, wreck_id))
            } else {
                false
            };
            (ship_or_st, foreign_deployable, locked_cont)
        });
    if is_ship_or_station {
        return Err(BridgeRpcError {
            code: "InvalidContainerType".into(),
            message: format!(
                "Item {wreck_id} (type {}) is not a lootable wreck or cargo container",
                wreck_item.type_id
            ),
        });
    }
    if is_foreign_deployable {
        return Err(BridgeRpcError {
            code: "AccessDenied".into(),
            message: format!("Cannot loot foreign deployable {wreck_id}"),
        });
    }
    if is_locked_container {
        return Err(BridgeRpcError {
            code: "ContainerLocked".into(),
            message: format!(
                "Secure container {wreck_id} requires a password before looting"
            ),
        });
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let active_ball_id = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
        .unwrap_or(ship_id);
    let ship_ball_opt = match sys_handle.get_ball_snapshot(ship_id).await {
        Ok(Some(v)) => Some(v),
        _ => sys_handle.get_ball_snapshot(active_ball_id).await.ok().flatten(),
    };
    let Some((_, sb)) = ship_ball_opt else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship ball is not present in space".into(),
        });
    };
    let Ok(Some((_, wb))) = sys_handle.get_ball_snapshot(wreck_id).await else {
        return Err(BridgeRpcError {
            code: "ContainerNotInSpace".into(),
            message: format!("Wreck or container {wreck_id} ball is not present in space"),
        });
    };
    let dist = sb.position.distance(wb.position);
    if !dist.is_finite() || dist > 2_500.0 {
        return Err(BridgeRpcError {
            code: "OutOfRange".into(),
            message: format!(
                "Wreck or container {wreck_id} is {dist:.1} m away (must be within 2,500 m)"
            ),
        });
    }

    let is_abandoned_rt = with_combat_runtime(ctx, |rt| {
        rt.container_lifetimes
            .get(&wreck_id)
            .map(|c| c.is_abandoned)
            .unwrap_or(false)
    });
    let safety = with_combat_runtime(ctx, |rt| {
        rt.safety_levels
            .get(&char_id)
            .copied()
            .unwrap_or(SafetyLevel::Red)
    });
    let wreck_owner_is_criminal = ctx
        .crimewatch
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_create(wreck_item.owner_id)
        .is_criminal(now_unix);

    let (looted_ids, remaining_ids, suspect_triggered, is_abandoned) = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get(&char_id) {
            let cur_db_state = (ch.station_id, ch.structure_id, ch.solar_system_id, ch.active_ship_id);
            if entry_db_state.is_some_and(|e| cur_db_state != e) {
                return Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: format!("Ship {ship_id} is no longer undocked in solar system {sys_id}"),
                });
            }
        }
        let Some(live_wreck) = snap.items.get(&wreck_id).cloned() else {
            return Err(BridgeRpcError {
                code: "ContainerNotFound".into(),
                message: format!("Wreck or container {wreck_id} no longer exists"),
            });
        };
        if live_wreck.location_id != sys_id as i64 {
            return Err(BridgeRpcError {
                code: "ContainerNotInSpace".into(),
                message: format!("Wreck or container {wreck_id} is no longer in space"),
            });
        }
        if live_wreck.owner_id != wreck_item.owner_id {
            return Err(BridgeRpcError {
                code: "ContainerOwnerChanged".into(),
                message: format!("Wreck or container {wreck_id} owner changed concurrently"),
            });
        }

        let is_abandoned = wreck_owner_is_criminal || live_wreck.owner_id == 0 || is_abandoned_rt;
        let c1 = snap.characters.get(&char_id).map(|c| c.corporation_id);
        let c2 = snap.characters.get(&live_wreck.owner_id).map(|c| c.corporation_id);
        let same_corp = c1.is_some() && c1 == c2 && c1.unwrap_or(0) > 2_000_000;
        let same_fleet = snap
            .fleets
            .values()
            .any(|f| f.members.contains(&char_id) && f.members.contains(&live_wreck.owner_id));
        let same_corp_or_fleet = same_corp || same_fleet;
        let needs_suspect = (live_wreck.owner_id != char_id || active_ball_id != ship_id)
            && !is_abandoned
            && !same_corp_or_fleet;
        if needs_suspect && safety == SafetyLevel::Green {
            return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
        }

        let ship_cap = snap
            .items
            .get(&ship_id)
            .and_then(|s| ctx.sde.types.get(&s.type_id))
            .map(|t| t.capacity)
            .unwrap_or(400.0);
        let cs_state = crate::services::inventory_fitting::fitting_containers_ships::read_container_ship_state_snap(snap);
        let used_vol: f64 = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo)
            .map(|i| {
                let unit_vol = ctx
                    .sde
                    .types
                    .get(&i.type_id)
                    .map(|t| t.volume)
                    .unwrap_or(1.0);
                unit_vol * (i.quantity.max(1) as f64)
            })
            .sum();
        let mut avail_vol = (ship_cap - used_vol).max(0.0);

        let mut wreck_children: Vec<i64> = snap
            .items
            .values()
            .filter(|i| i.location_id == wreck_id)
            .map(|i| i.item_id)
            .collect();
        wreck_children.sort_unstable();

        let mut looted = Vec::new();
        let mut remaining = Vec::new();
        for cid in wreck_children {
            let Some(child) = snap.items.get(&cid).cloned() else {
                continue;
            };
            if child.flag == InventoryFlag::Locked || cs_state.alsc_locked_items.contains(&cid) {
                remaining.push(cid);
                continue;
            }
            let unit_vol = ctx
                .sde
                .types
                .get(&child.type_id)
                .map(|t| t.volume)
                .unwrap_or(1.0)
                .max(0.0001);
            let total_vol = unit_vol * (child.quantity.max(1) as f64);
            if total_vol <= avail_vol + 1e-6 {
                avail_vol = (avail_vol - total_vol).max(0.0);
                if let Some(c_mut) = snap.items.get_mut(&cid) {
                    c_mut.location_id = ship_id;
                    c_mut.owner_id = char_id;
                    c_mut.flag = InventoryFlag::Cargo;
                    c_mut.version += 1;
                }
                looted.push(cid);
            } else {
                let max_units = (avail_vol / unit_vol).floor() as i32;
                if max_units > 0 && child.quantity > max_units && !child.singleton {
                    avail_vol = (avail_vol - (max_units as f64) * unit_vol).max(0.0);
                    if let Some(c_mut) = snap.items.get_mut(&cid) {
                        c_mut.quantity -= max_units;
                        c_mut.version += 1;
                    }
                    snap.next_entity_id += 1;
                    let split_id = snap.next_entity_id;
                    snap.items.insert(
                        split_id,
                        InventoryItemRecord {
                            item_id: split_id,
                            type_id: child.type_id,
                            owner_id: char_id,
                            location_id: ship_id,
                            flag: InventoryFlag::Cargo,
                            quantity: max_units,
                            singleton: child.singleton,
                            version: 1,
                            dynamic_attributes: child.dynamic_attributes.clone(),
                            source_mutaplasmid_id: child.source_mutaplasmid_id,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: child.is_blueprint_copy,
                            blueprint_runs_remaining: child.blueprint_runs_remaining,
                        },
                    );
                    looted.push(split_id);
                }
                remaining.push(cid);
            }
        }
        Ok((looted, remaining, needs_suspect, is_abandoned))
    })?;

    if suspect_triggered {
        {
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let st = cw.get_or_create(char_id);
            st.suspect_flag_until_unix = now_unix.saturating_add(900);
        }
        let _ = ctx.notify_system_criminal_flag_updates(sys_id, &[], &[char_id], &[]);
    }

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnWreckLootAll",
        "charid",
        vec![
            PyRep::Integer(wreck_id),
            PyRep::List(looted_ids.iter().copied().map(PyRep::Integer).collect()),
        ],
    );
    let looted_types: Vec<u32> = ctx.db.with_snapshot(|snap| {
        looted_ids
            .iter()
            .filter_map(|lid| snap.items.get(lid).map(|it| it.type_id))
            .collect()
    });
    for ltid in looted_types {
        crate::services::pve_dungeon_runtime::on_dungeon_item_looted(
            ctx,
            char_id,
            Some(sys_id),
            wreck_id,
            ltid,
        )
        .await;
    }
    if remaining_ids.is_empty() && wreck_item.type_id == 23 {
        let _ = super::despawn_empty_jetcan_if_needed(ctx, Some(sys_id), wreck_id).await;
    }
    Ok(PyRep::keyval([
        ("wreckID", PyRep::Integer(wreck_id)),
        (
            "lootedItemIDs",
            PyRep::List(looted_ids.into_iter().map(PyRep::Integer).collect()),
        ),
        (
            "remainingItemIDs",
            PyRep::List(remaining_ids.into_iter().map(PyRep::Integer).collect()),
        ),
        ("suspectFlagTriggered", PyRep::Bool(suspect_triggered)),
        ("abandoned", PyRep::Bool(is_abandoned)),
    ]))
}

pub(crate) fn authorize_killmail_read(
    session: &SessionState,
    char_id: i64,
    snap: &eve_db::WorldStateSnapshot,
    expected_hash: &str,
    requested_hash: Option<&str>,
    kill_id: i64,
    victim_char_id: i64,
    victim_corp_id: i64,
    victim_ally_id: Option<i64>,
    attackers: &[(i64, i64, Option<i64>)],
    kill_time_unix: u64,
) -> Result<(), BridgeRpcError> {
    if let Some(req_h) = requested_hash.filter(|h| !h.is_empty()) {
        if req_h != expected_hash {
            return Err(BridgeRpcError {
                code: "InvalidKillmailHash".into(),
                message: format!("Killmail hash mismatch for kill {kill_id}"),
            });
        }
        return Ok(());
    }
    if session.has_gm_role() {
        return Ok(());
    }
    if char_id > 0 && (victim_char_id == char_id || attackers.iter().any(|&(c, _, _)| c == char_id)) {
        return Ok(());
    }
    let is_npc_corp = |cid: i64| (1_000_000..2_000_000).contains(&cid);
    let caller_corp = session
        .corporation_id
        .filter(|&c| c > 0)
        .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id))
        .unwrap_or(0);
    let resolve_corp = |cid: i64, explicit: i64| {
        if explicit > 0 {
            explicit
        } else {
            snap.characters.get(&cid).map(|c| c.corporation_id).unwrap_or(0)
        }
    };
    let vic_corp = resolve_corp(victim_char_id, victim_corp_id);
    let att_corps: Vec<i64> = attackers
        .iter()
        .map(|&(ac, acorp, _)| resolve_corp(ac, acorp))
        .collect();
    if caller_corp > 0
        && !is_npc_corp(caller_corp)
        && (caller_corp == vic_corp || att_corps.contains(&caller_corp))
    {
        return Ok(());
    }
    let caller_ally = session
        .alliance_id
        .filter(|&a| a > 0)
        .or_else(|| snap.characters.get(&char_id).and_then(|c| c.alliance_id))
        .or_else(|| {
            (caller_corp > 0)
                .then(|| snap.corporations.get(&caller_corp).and_then(|c| c.alliance_id))
                .flatten()
        })
        .filter(|&a| a > 0);
    if let Some(c_ally) = caller_ally {
        let resolve_ally = |explicit_ally: Option<i64>, corp_id: i64| {
            explicit_ally
                .filter(|&a| a > 0)
                .or_else(|| snap.corporations.get(&corp_id).and_then(|c| c.alliance_id))
        };
        if resolve_ally(victim_ally_id, vic_corp) == Some(c_ally)
            || attackers
                .iter()
                .zip(att_corps.iter())
                .any(|(&(_, _, aally), &rc)| resolve_ally(aally, rc) == Some(c_ally))
        {
            return Ok(());
        }
    }
    if caller_corp > 0 && !is_npc_corp(caller_corp) {
        for (war_id, w) in &snap.wars {
            let meta = snap.war_state.wars.get(war_id);
            if kill_time_unix > 0 {
                if kill_time_unix < w.started_unix {
                    continue;
                }
                if meta.and_then(|m| m.finish_at_unix).is_some_and(|fin| kill_time_unix > fin) {
                    continue;
                }
            }
            let (agg, def) = (w.aggressor_corp_id, w.defender_corp_id);
            let allies: Vec<i64> = snap
                .war_state
                .negotiations
                .iter()
                .filter(|n| {
                    n.war_id == *war_id
                        && n.kind == eve_db::PersistedNegKind::Ally
                        && n.status == "Accepted"
                        && (kill_time_unix == 0 || kill_time_unix >= n.effective_unix.unwrap_or(0))
                })
                .map(|n| n.from_corp)
                .collect();
            let is_def_side = |cid: i64| cid == def || allies.contains(&cid);
            let is_war_kill = (vic_corp == agg && att_corps.iter().any(|&c| is_def_side(c)))
                || (is_def_side(vic_corp) && att_corps.contains(&agg));
            if is_war_kill && (caller_corp == agg || is_def_side(caller_corp)) {
                return Ok(());
            }
        }
    }
    Err(BridgeRpcError {
        code: "KillmailAccessDenied".into(),
        message: format!("Not authorized to view killmail {kill_id} without a valid killmail hash"),
    })
}

pub(super) fn handle_get_killmail(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(kill_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "GetKillMail requires killID".into(),
        });
    };
    let requested_hash = args.get(1).and_then(|a| a.as_str());

    let detailed_opt = with_combat_runtime(ctx, |rt| rt.detailed_killmails.get(&kill_id).cloned());
    if let Some(dkm) = detailed_opt {
        let attackers_ref: Vec<(i64, i64, Option<i64>)> = dkm
            .attackers
            .iter()
            .map(|a| (a.character_id, a.corporation_id, a.alliance_id))
            .collect();
        ctx.db.with_snapshot(|snap| {
            authorize_killmail_read(
                session,
                char_id,
                snap,
                &dkm.killmail_hash,
                requested_hash,
                kill_id,
                dkm.victim_character_id,
                dkm.victim_corporation_id,
                dkm.victim_alliance_id,
                &attackers_ref,
                dkm.kill_time_unix,
            )
        })?;
        let attackers_list: Vec<PyRep> = dkm
            .attackers
            .iter()
            .map(|a| {
                PyRep::keyval([
                    ("characterID", PyRep::Integer(a.character_id)),
                    ("corporationID", PyRep::Integer(a.corporation_id)),
                    (
                        "allianceID",
                        a.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                    ("shipTypeID", PyRep::Integer(a.ship_type_id as i64)),
                    ("weaponTypeID", PyRep::Integer(a.weapon_type_id as i64)),
                    ("damageDone", PyRep::Real(a.damage_done)),
                    ("finalBlow", PyRep::Bool(a.final_blow)),
                    ("securityStatus", PyRep::Real(a.security_status)),
                ])
            })
            .collect();
        let dropped_list: Vec<PyRep> = dkm
            .dropped_items
            .iter()
            .map(|i| {
                PyRep::keyval([
                    ("typeID", PyRep::Integer(i.type_id as i64)),
                    ("flag", PyRep::Integer(i.flag as i64)),
                    ("qtyDropped", PyRep::Integer(i.qty_dropped as i64)),
                    ("qtyDestroyed", PyRep::Integer(0)),
                    ("singleton", PyRep::Bool(i.singleton)),
                ])
            })
            .collect();
        let destroyed_list: Vec<PyRep> = dkm
            .destroyed_items
            .iter()
            .map(|i| {
                PyRep::keyval([
                    ("typeID", PyRep::Integer(i.type_id as i64)),
                    ("flag", PyRep::Integer(i.flag as i64)),
                    ("qtyDropped", PyRep::Integer(0)),
                    ("qtyDestroyed", PyRep::Integer(i.qty_destroyed as i64)),
                    ("singleton", PyRep::Bool(i.singleton)),
                ])
            })
            .collect();

        let final_char_id = dkm
            .attackers
            .iter()
            .find(|a| a.final_blow)
            .or_else(|| dkm.attackers.first())
            .map(|a| a.character_id)
            .unwrap_or(dkm.victim_character_id);
        return Ok(PyRep::keyval([
            ("killID", PyRep::Integer(dkm.kill_id)),
            ("killTime", PyRep::Integer(dkm.kill_time_unix as i64)),
            ("solarSystemID", PyRep::Integer(dkm.solar_system_id as i64)),
            ("hash", PyRep::String(dkm.killmail_hash)),
            ("victimCharacterID", PyRep::Integer(dkm.victim_character_id)),
            ("finalCharacterID", PyRep::Integer(final_char_id)),
            (
                "victimCorporationID",
                PyRep::Integer(dkm.victim_corporation_id),
            ),
            (
                "victimAllianceID",
                dkm.victim_alliance_id
                    .map(PyRep::Integer)
                    .unwrap_or(PyRep::None),
            ),
            (
                "victimShipTypeID",
                PyRep::Integer(dkm.victim_ship_type_id as i64),
            ),
            ("victimDamageTaken", PyRep::Real(dkm.victim_damage_taken)),
            (
                "victimPosition",
                PyRep::Tuple(vec![
                    PyRep::Real(dkm.victim_position[0]),
                    PyRep::Real(dkm.victim_position[1]),
                    PyRep::Real(dkm.victim_position[2]),
                ]),
            ),
            ("attackers", PyRep::List(attackers_list)),
            ("droppedItems", PyRep::List(dropped_list)),
            ("destroyedItems", PyRep::List(destroyed_list)),
            ("totalValueISK", PyRep::Real(dkm.total_value_isk)),
            ("iskLost", PyRep::Real(dkm.total_value_isk)),
        ]));
    }

    let basic_opt = ctx
        .db
        .with_snapshot(|snap| snap.killmails.get(&kill_id).cloned());
    let Some(km) = basic_opt else {
        return Err(BridgeRpcError {
            code: "KillmailNotFound".into(),
            message: format!("Killmail {kill_id} not found"),
        });
    };
    let kill_ts = if km.killed_unix > 0 {
        km.killed_unix
    } else {
        ctx.now_unix()
    };
    let hash = super::compute_killmail_esi_hash(
        km.victim_character_id,
        km.attacker_character_id,
        km.ship_type_id,
        kill_ts,
    );
    ctx.db.with_snapshot(|snap| {
        authorize_killmail_read(
            session,
            char_id,
            snap,
            &hash,
            requested_hash,
            kill_id,
            km.victim_character_id,
            km.victim_corp_id,
            None,
            &[(km.attacker_character_id, km.attacker_corp_id, None)],
            km.killed_unix,
        )
    })?;
    Ok(PyRep::keyval([
        ("killID", PyRep::Integer(km.kill_id)),
        ("killTime", PyRep::Integer(kill_ts as i64)),
        ("solarSystemID", PyRep::Integer(km.solar_system_id as i64)),
        ("hash", PyRep::String(hash)),
        ("victimCharacterID", PyRep::Integer(km.victim_character_id)),
        ("finalCharacterID", PyRep::Integer(km.attacker_character_id)),
        ("victimShipTypeID", PyRep::Integer(km.ship_type_id as i64)),
        (
            "attackers",
            PyRep::List(vec![PyRep::keyval([
                ("characterID", PyRep::Integer(km.attacker_character_id)),
                ("finalBlow", PyRep::Bool(true)),
            ])]),
        ),
        ("droppedItems", PyRep::List(Vec::new())),
        ("destroyedItems", PyRep::List(Vec::new())),
        ("totalValueISK", PyRep::Real(km.total_value_isk)),
        ("iskLost", PyRep::Real(km.total_value_isk)),
    ]))
}
