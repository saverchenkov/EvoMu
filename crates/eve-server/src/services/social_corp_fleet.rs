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
        ("bookmark", "BookmarkLocation") | ("bookmarkMgr", "BookmarkLocation") => {
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
            let tax_rate = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.10);

            let corp_id = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let cid = 98_000_000 + snap.next_entity_id;
                snap.corporations.insert(
                    cid,
                    CorporationRecord {
                        corporation_id: cid,
                        name: name.to_string(),
                        ticker: ticker.to_string(),
                        ceo_id: char_id,
                        alliance_id: None,
                        tax_rate,
                        master_wallet: IskCents::from_isk(100_000_000),
                        members: HashSet::from([char_id]),
                    },
                );
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.corporation_id = cid;
                }
                cid
            });

            session.corporation_id = Some(corp_id);
            Some(Ok(PyRep::keyval([
                ("corporationID", PyRep::Integer(corp_id)),
                ("name", PyRep::String(name.to_string())),
                ("ticker", PyRep::String(ticker.to_string())),
            ])))
        }

        ("corporation_projects", "CreateProject") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let name = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("Deliver Tritanium");
            let target_qty = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1_000) as u64;
            let isk_per_unit = args.get(2).and_then(|a| a.as_f64()).unwrap_or(10.0);
            let corp_id = session.corporation_id.unwrap_or(1_000_125);

            let pid = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                let per_u = IskCents::from_f64_isk(isk_per_unit);
                let escrow = IskCents(per_u.0 * (target_qty as i64));
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
                let _ = char_id;
                id
            });
            Some(Ok(PyRep::keyval([
                ("projectID", PyRep::Integer(pid)),
                ("targetQuantity", PyRep::Integer(target_qty as i64)),
            ])))
        }

        ("corporation_projects", "ContributeToProject") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let pid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let qty = args.get(1).and_then(|a| a.as_i64()).unwrap_or(100) as u64;

            let (payout_isk, completed) = ctx.db.with_mut_snapshot(|snap| {
                let Some(proj) = snap.corp_projects.get_mut(&pid) else {
                    return (0.0, false);
                };
                let accepted = qty.min(proj.target_quantity.saturating_sub(proj.current_quantity));
                proj.current_quantity += accepted;
                let reward = IskCents(proj.isk_per_unit.0 * (accepted as i64));
                proj.escrow_remaining = IskCents((proj.escrow_remaining.0 - reward.0).max(0));
                let done = proj.current_quantity >= proj.target_quantity;
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = IskCents(ch.balance.0 + reward.0);
                }
                (reward.to_wire_f64(), done)
            });

            Some(Ok(PyRep::keyval([
                ("projectID", PyRep::Integer(pid)),
                ("payoutISK", PyRep::Real(payout_isk)),
                ("completed", PyRep::Bool(completed)),
            ])))
        }

        ("LSC", _)
        | ("chatx", _)
        | ("notificationSvc", _)
        | ("notificationMgr", _)
        | ("calendarMgr", _)
        | ("calendarProxy", _)
        | ("fleetProxy", _)
        | ("allianceRegistry", _)
        | ("petitioner", _)
        | ("voiceMgr", _)
        | ("addressbook", _)
        | ("contactMgr", _)
        | ("overviewPresetMgr", _) => Some(Ok(PyRep::Bool(true))),

        _ => None,
    }
}
