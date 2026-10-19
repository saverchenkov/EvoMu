//! `social_corp_fleet` — Domain 10: LSC Chat, EveMail, Bookmarks, Fleets, Corporations, & Corp Projects (`§3.11`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{
    AllianceRecord, BookmarkRecord, CorpProjectRecord, CorporationRecord, FleetRecord, IskCents,
    MailRecord, WarRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("bookmark", "BookmarkLocation") | ("accessGroupBookmarkMgr", "BookmarkStaticLocation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let label = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("Safe Spot")
                .trim();
            if label.is_empty() || label.chars().count() > 100 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidBookmarkLabel".into(),
                    message: "Bookmark label must be 1..=100 non-whitespace characters".into(),
                }));
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };

            // Derive bookmark coordinates strictly from the pilot's docked station/structure or server-side ship ball (never client args)
            let (x, y, z) = if let Some(st_id) = session.station_id {
                ctx.sde
                    .solar_systems
                    .get(&sys_id)
                    .and_then(|s| s.stations.iter().find(|st| st.station_id == st_id))
                    .map(|st| (st.position.x, st.position.y, st.position.z))
                    .unwrap_or((0.0, 0.0, 0.0))
            } else if let Some(struct_id) = session.structure_id {
                ctx.db
                    .with_snapshot(|snap| {
                        snap.structures
                            .get(&struct_id)
                            .map(|s| (s.position[0], s.position[1], s.position[2]))
                    })
                    .unwrap_or((0.0, 0.0, 0.0))
            } else if let Some(ship_id) = session.ship_id {
                let handle = ctx.systems.get_or_spawn(sys_id);
                if let Ok(Some((_, ball))) = handle.get_ball_snapshot(ship_id).await {
                    (ball.position.x, ball.position.y, ball.position.z)
                } else {
                    return Some(Err(BridgeRpcError {
                        code: "ShipNotInSpace".into(),
                        message: "Cannot bookmark in-space position when ship ball is absent"
                            .into(),
                    }));
                }
            } else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let count = snap
                    .bookmarks
                    .values()
                    .filter(|b| b.owner_id == char_id)
                    .count();
                if count >= 500 {
                    return Err(BridgeRpcError {
                        code: "MaxBookmarksReached".into(),
                        message: "Character has reached the maximum of 500 personal bookmarks".into(),
                    });
                }
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.bookmarks.insert(
                    id,
                    BookmarkRecord {
                        bookmark_id: id,
                        owner_id: char_id,
                        label: label.to_string(),
                        solar_system_id: sys_id,
                        x,
                        y,
                        z,
                        item_id: None,
                        memo: String::new(),
                    },
                );
                Ok(id)
            });
            match res {
                Ok(bm_id) => Some(Ok(PyRep::keyval([
                    ("bookmarkID", PyRep::Integer(bm_id)),
                    ("label", PyRep::String(label.to_string())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "GetBookmarks") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.bookmarks
                    .values()
                    .filter(|b| b.owner_id == char_id)
                    .map(|b| {
                        vec![
                            PyRep::Integer(b.bookmark_id),
                            PyRep::String(b.label.clone()),
                            PyRep::Integer(b.solar_system_id as i64),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["bookmarkID", "label", "solarSystemID"],
                rows,
            )))
        }

        ("mailMgr", "SendMail") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(recipient_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidRecipient".into(),
                    message: "SendMail requires recipientID".into(),
                }));
            };
            let subject = args.get(1).and_then(|a| a.as_str()).unwrap_or("o7");
            let body = args.get(2).and_then(|a| a.as_str()).unwrap_or("Fly safe!");
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if snap.mails.len() >= 1_000 {
                    return Err(BridgeRpcError {
                        code: "MailQuotaExceeded".into(),
                        message: "Server mail spool limit (1,000 messages) reached".into(),
                    });
                }
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.mails.push(MailRecord {
                    message_id: id,
                    sender_id: char_id,
                    recipient_id,
                    subject: subject.to_string(),
                    body: body.to_string(),
                });
                Ok(id)
            });
            match res {
                Ok(mid) => Some(Ok(PyRep::Integer(mid))),
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetMgr", "CreateFleet") | ("fleetObjectHandler", "CreateFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    snap.next_entity_id += 1;
                    let id = snap.next_entity_id;
                    snap.fleets.insert(
                        id,
                        FleetRecord {
                            fleet_id: id,
                            boss_id: char_id,
                            members: vec![char_id],
                            wings: vec![1],
                            squads: vec![1],
                            free_move: true,
                            is_registered: true,
                            ..Default::default()
                        },
                    );
                    Ok::<_, std::convert::Infallible>(id)
                })
                .unwrap();
            session.fleet_id = Some(fid);
            Some(Ok(PyRep::keyval([
                ("fleetID", PyRep::Integer(fid)),
                ("bossID", PyRep::Integer(char_id)),
            ])))
        }

        ("corpmgr", "CreateCorporation") | ("corpRegistry", "AddCorporation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let name = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("EvoMu Industrial Corp");
            let ticker = args.get(1).and_then(|a| a.as_str()).unwrap_or("EVOMU");
            let tax_rate = args
                .get(2)
                .and_then(|a| a.as_f64())
                .unwrap_or(0.10)
                .clamp(0.0, 1.0);
            let reg_fee = IskCents::from_isk(1_599_800);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let new_bal = ch.balance.checked_sub(reg_fee).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Not enough ISK to pay corporation registration fee (1,599,800 ISK)"
                        .into(),
                })?;
                ch.balance = new_bal;

                snap.next_entity_id += 1;
                let cid = 98_000_000 + snap.next_entity_id;
                if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                    ch_mut.corporation_id = cid;
                    ch_mut.alliance_id = None;
                }
                snap.corporations.insert(
                    cid,
                    CorporationRecord {
                        corporation_id: cid,
                        name: name.to_string(),
                        ticker: ticker.to_string(),
                        ceo_id: char_id,
                        alliance_id: None,
                        tax_rate,
                        master_wallet: IskCents(0),
                        members: HashSet::from([char_id]),
                        ..Default::default()
                    },
                );
                Ok(cid)
            });

            match res {
                Ok(corp_id) => {
                    session.corporation_id = Some(corp_id);
                    session.alliance_id = None;
                    session.corp_role = u64::MAX;
                    Some(Ok(PyRep::keyval([
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("allianceID", PyRep::None),
                        ("name", PyRep::String(name.to_string())),
                        ("ticker", PyRep::String(ticker.to_string())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("allianceRegistry", "CreateAlliance") | ("corpRegistry", "CreateAlliance") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let name = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("EvoMu Sovereign Coalition");
            let short_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("EVOSC");
            let alliance_fee = IskCents::from_isk(10_000_000);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(0);
                if corp_id < 2_000_000 {
                    return Err(BridgeRpcError {
                        code: "NpcCorporationCannotFormAlliance".into(),
                        message: "Only player-founded corporations can form an alliance".into(),
                    });
                }
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                if corp.ceo_id != char_id {
                    return Err(BridgeRpcError {
                        code: "CeoRoleRequired".into(),
                        message: "Only the corporation CEO can form an alliance".into(),
                    });
                }
                if corp.alliance_id.is_some() {
                    return Err(BridgeRpcError {
                        code: "AlreadyInAlliance".into(),
                        message: "Corporation is already a member of an alliance".into(),
                    });
                }
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let new_bal =
                    ch.balance
                        .checked_sub(alliance_fee)
                        .ok_or_else(|| BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: "Not enough ISK to pay alliance registration fee".into(),
                        })?;
                ch.balance = new_bal;

                snap.next_entity_id += 1;
                let aid = 99_000_000 + snap.next_entity_id;
                let member_ids: Vec<i64> = snap
                    .corporations
                    .get(&corp_id)
                    .map(|c| c.members.iter().copied().collect())
                    .unwrap_or_else(|| vec![char_id]);
                if let Some(corp_mut) = snap.corporations.get_mut(&corp_id) {
                    corp_mut.alliance_id = Some(aid);
                }
                for mid in member_ids {
                    if let Some(c_mut) = snap.characters.get_mut(&mid) {
                        c_mut.alliance_id = Some(aid);
                    }
                }
                snap.alliances.insert(
                    aid,
                    AllianceRecord {
                        alliance_id: aid,
                        name: name.to_string(),
                        short_name: short_name.to_string(),
                        executor_corp_id: corp_id,
                        member_corp_ids: HashSet::from([corp_id]),
                    },
                );
                Ok((aid, corp_id))
            });

            match res {
                Ok((alliance_id, corp_id)) => {
                    session.alliance_id = Some(alliance_id);
                    Some(Ok(PyRep::keyval([
                        ("allianceID", PyRep::Integer(alliance_id)),
                        ("executorCorpID", PyRep::Integer(corp_id)),
                        ("allianceName", PyRep::String(name.to_string())),
                        ("shortName", PyRep::String(short_name.to_string())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("allianceRegistry", "GetAlliance") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let aid = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.alliance_id)
                .unwrap_or(0);
            let alliance_opt = ctx
                .db
                .with_snapshot(|snap| snap.alliances.get(&aid).cloned());
            let Some(al) = alliance_opt else {
                return Some(Err(BridgeRpcError {
                    code: "AllianceNotFound".into(),
                    message: format!("Alliance {aid} does not exist"),
                }));
            };
            Some(Ok(PyRep::keyval([
                ("allianceID", PyRep::Integer(al.alliance_id)),
                ("allianceName", PyRep::String(al.name)),
                ("shortName", PyRep::String(al.short_name)),
                ("executorCorpID", PyRep::Integer(al.executor_corp_id)),
            ])))
        }

        ("warRegistry", "DeclareWarAgainst") | ("corpRegistry", "DeclareWarAgainst") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let defender_corp_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let war_fee = IskCents::from_isk(10_000_000);
            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let aggressor_corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(0);
                if aggressor_corp_id < 2_000_000 {
                    return Err(BridgeRpcError {
                        code: "NpcCorporationCannotDeclareWar".into(),
                        message: "NPC corporations cannot declare war".into(),
                    });
                }
                if defender_corp_id < 2_000_000 || !snap.corporations.contains_key(&defender_corp_id) {
                    return Err(BridgeRpcError {
                        code: "InvalidWarTarget".into(),
                        message: format!(
                            "Target corporation {defender_corp_id} is not a valid player corporation"
                        ),
                    });
                }
                if aggressor_corp_id == defender_corp_id {
                    return Err(BridgeRpcError {
                        code: "CannotDeclareWarOnSelf".into(),
                        message: "Cannot declare war against your own corporation".into(),
                    });
                }
                let Some(agg_corp) = snap.corporations.get(&aggressor_corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {aggressor_corp_id} not found"),
                    });
                };
                if agg_corp.ceo_id != char_id {
                    return Err(BridgeRpcError {
                        code: "CeoRoleRequired".into(),
                        message: "Only the corporation CEO can declare war".into(),
                    });
                }
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let new_bal = ch.balance.checked_sub(war_fee).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Not enough ISK to pay CONCORD war declaration fee (10,000,000 ISK)"
                        .into(),
                })?;
                ch.balance = new_bal;

                snap.next_entity_id += 1;
                let war_id = snap.next_entity_id;
                snap.wars.insert(
                    war_id,
                    WarRecord {
                        war_id,
                        aggressor_corp_id,
                        defender_corp_id,
                        mutual: false,
                        started_unix: now_unix,
                        active: true,
                    },
                );
                Ok((war_id, aggressor_corp_id))
            });

            match res {
                Ok((war_id, aggressor_corp_id)) => Some(Ok(PyRep::keyval([
                    ("warID", PyRep::Integer(war_id)),
                    ("aggressorCorpID", PyRep::Integer(aggressor_corp_id)),
                    ("defenderCorpID", PyRep::Integer(defender_corp_id)),
                    ("active", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("warRegistry", "GetWars") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.corporation_id)
                .unwrap_or(0);
            let rows = ctx.db.with_snapshot(|snap| {
                snap.wars
                    .values()
                    .filter(|w| {
                        corp_id == 0
                            || w.aggressor_corp_id == corp_id
                            || w.defender_corp_id == corp_id
                    })
                    .map(|w| {
                        vec![
                            PyRep::Integer(w.war_id),
                            PyRep::Integer(w.aggressor_corp_id),
                            PyRep::Integer(w.defender_corp_id),
                            PyRep::Bool(w.active),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["warID", "aggressorCorpID", "defenderCorpID", "active"],
                rows,
            )))
        }

        ("corporationSvc", "CreateProject") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let name = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("Deliver Tritanium");
            let target_qty_raw = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1_000);
            let isk_per_unit = args.get(2).and_then(|a| a.as_f64()).unwrap_or(10.0);
            if target_qty_raw <= 0 || !isk_per_unit.is_finite() || isk_per_unit < 0.0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidProjectParameters".into(),
                    message: "Project requires target_quantity > 0 and finite isk_per_unit >= 0"
                        .into(),
                }));
            }
            let target_qty = target_qty_raw as u64;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let per_u = IskCents::from_f64_isk(isk_per_unit);
                let escrow =
                    per_u
                        .checked_mul_qty(target_qty_raw)
                        .ok_or_else(|| BridgeRpcError {
                            code: "ProjectEscrowOverflow".into(),
                            message: "Corporation project total escrow overflows 64-bit ISK cents"
                                .into(),
                        })?;

                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let corp_id = session.corporation_id.unwrap_or(ch.corporation_id);
                let new_bal = ch.balance.checked_sub(escrow).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Not enough ISK to fund corporation project escrow".into(),
                })?;
                ch.balance = new_bal;

                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.corp_projects.insert(
                    id,
                    CorpProjectRecord {
                        project_id: id,
                        corporation_id: corp_id,
                        name: name.to_string(),
                        target_quantity: target_qty,
                        current_quantity: 0,
                        isk_per_unit: per_u,
                        escrow_remaining: escrow,
                    },
                );
                Ok(id)
            });

            match res {
                Ok(pid) => Some(Ok(PyRep::keyval([
                    ("projectID", PyRep::Integer(pid)),
                    ("targetQuantity", PyRep::Integer(target_qty as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("corporationSvc", "ContributeToProject") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let pid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let qty_raw = args.get(1).and_then(|a| a.as_i64()).unwrap_or(100);
            if qty_raw <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidContributionQuantity".into(),
                    message: "Contribution quantity must be > 0".into(),
                }));
            }
            let qty = qty_raw as u64;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(proj) = snap.corp_projects.get(&pid).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ProjectNotFound".into(),
                        message: format!("Corporation project {pid} does not exist"),
                    });
                };
                let char_corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(0);
                if proj.corporation_id != char_corp_id {
                    return Err(BridgeRpcError {
                        code: "NotInProjectCorporation".into(),
                        message: format!(
                            "Character {char_id} (corp {char_corp_id}) is not a member of project corporation {}",
                            proj.corporation_id
                        ),
                    });
                }
                let needed = proj.target_quantity.saturating_sub(proj.current_quantity);
                let accepted = qty.min(needed);
                if accepted == 0 {
                    return Err(BridgeRpcError {
                        code: "ProjectAlreadyCompleted".into(),
                        message: format!("Corporation project {pid} is already completed"),
                    });
                }

                // Require and consume `accepted` units of unlocked, reachable Veldspar (1230), Compressed Veldspar (62516), or Tritanium (34)
                let current_dock = session.station_id.or(session.structure_id);
                let current_ship = session.ship_id;
                let contrib_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        matches!(i.type_id, 1230 | 62516 | 34)
                            && (i.quantity as u64) >= accepted
                            && snap.is_item_consumable_by(i, char_id, current_dock, current_ship)
                    })
                    .map(|i| i.item_id);
                let Some(iid) = contrib_item_id else {
                    return Err(BridgeRpcError {
                        code: "InsufficientContributionItems".into(),
                        message: format!(
                            "Character {char_id} does not have {accepted} units of ore/mineral to contribute"
                        ),
                    });
                };

                if let Some(item) = snap.items.get_mut(&iid) {
                    if (item.quantity as u64) > accepted {
                        item.quantity -= accepted as i32;
                        item.version += 1;
                    } else {
                        snap.items.remove(&iid);
                    }
                }

                let reward = proj
                    .isk_per_unit
                    .checked_mul_qty(accepted as i64)
                    .unwrap_or(IskCents(0));
                let mut done = false;
                if let Some(proj_mut) = snap.corp_projects.get_mut(&pid) {
                    proj_mut.current_quantity += accepted;
                    proj_mut.escrow_remaining = proj_mut
                        .escrow_remaining
                        .checked_sub(reward)
                        .unwrap_or(IskCents(0));
                    done = proj_mut.current_quantity >= proj_mut.target_quantity;
                }
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = ch.balance.checked_add(reward).unwrap_or(ch.balance);
                }
                Ok((reward.to_wire_f64(), done))
            });

            match res {
                Ok((payout_isk, completed)) => Some(Ok(PyRep::keyval([
                    ("projectID", PyRep::Integer(pid)),
                    ("payoutISK", PyRep::Real(payout_isk)),
                    ("completed", PyRep::Bool(completed)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
