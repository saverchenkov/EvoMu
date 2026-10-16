//! `social_corp_fleet` — Domain 10: LSC Chat, EveMail, Bookmarks, Fleets, Corporations, & Corp Projects (`§3.11`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{
    BookmarkRecord, CorpProjectRecord, CorporationRecord, FleetRecord, IskCents, MailRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

pub fn handle(
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
            let label = args.first().and_then(|a| a.as_str()).unwrap_or("Safe Spot");
            let x = args.get(1).and_then(|a| a.as_f64()).unwrap_or(10_000.0);
            let y = args.get(2).and_then(|a| a.as_f64()).unwrap_or(20_000.0);
            let z = args.get(3).and_then(|a| a.as_f64()).unwrap_or(30_000.0);
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);

            let bm_id = ctx.db.with_mut_snapshot(|snap| {
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
                    },
                );
                id
            });
            Some(Ok(PyRep::keyval([
                ("bookmarkID", PyRep::Integer(bm_id)),
                ("label", PyRep::String(label.to_string())),
            ])))
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
            let recipient_id = args.first().and_then(|a| a.as_i64()).unwrap_or(90_000_002);
            let subject = args.get(1).and_then(|a| a.as_str()).unwrap_or("o7");
            let body = args.get(2).and_then(|a| a.as_str()).unwrap_or("Fly safe!");
            let mid = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.mails.push(MailRecord {
                    message_id: id,
                    sender_id: char_id,
                    recipient_id,
                    subject: subject.to_string(),
                    body: body.to_string(),
                });
                id
            });
            Some(Ok(PyRep::Integer(mid)))
        }

        ("fleetMgr", "CreateFleet") | ("fleetObjectHandler", "CreateFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = ctx.db.with_mut_snapshot(|snap| {
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
                    },
                );
                id
            });
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
                    },
                );
                Ok(cid)
            });

            match res {
                Ok(corp_id) => {
                    session.corporation_id = Some(corp_id);
                    session.corp_role = u64::MAX;
                    Some(Ok(PyRep::keyval([
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("name", PyRep::String(name.to_string())),
                        ("ticker", PyRep::String(ticker.to_string())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
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
            let corp_id = session.corporation_id.unwrap_or(1_000_125);

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
                let needed = proj.target_quantity.saturating_sub(proj.current_quantity);
                let accepted = qty.min(needed);
                if accepted == 0 {
                    return Err(BridgeRpcError {
                        code: "ProjectAlreadyCompleted".into(),
                        message: format!("Corporation project {pid} is already completed"),
                    });
                }

                // Require and consume `accepted` units of Veldspar (1230), Compressed Veldspar (62516), or Tritanium (34)
                let contrib_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && matches!(i.type_id, 1230 | 62516 | 34)
                            && (i.quantity as u64) >= accepted
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

        ("corporationSvc", "GetCorpInfo") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.corporation_id)
                .unwrap_or(1_000_125);
            let corp_opt = ctx
                .db
                .with_snapshot(|snap| snap.corporations.get(&corp_id).cloned());
            if let Some(corp) = corp_opt {
                Some(Ok(PyRep::keyval([
                    ("corporationID", PyRep::Integer(corp.corporation_id)),
                    ("corporationName", PyRep::String(corp.name)),
                    ("ticker", PyRep::String(corp.ticker)),
                    ("ceoID", PyRep::Integer(corp.ceo_id)),
                    ("taxRate", PyRep::Real(corp.tax_rate)),
                ])))
            } else {
                Some(Ok(PyRep::keyval([
                    ("corporationID", PyRep::Integer(corp_id)),
                    ("corporationName", PyRep::String("NPC Corporation".into())),
                    ("ticker", PyRep::String("NPC".into())),
                ])))
            }
        }

        _ => None,
    }
}
