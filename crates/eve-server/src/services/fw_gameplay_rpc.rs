//! `fw_gameplay_rpc` — Live `facWarMgr` MachoNet RPC Handler, Militia Corp/Alliance Enlistment, Leaderboards & I-Hub LP Donations (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fw_gameplay::militia_lp_corp_id;
use crate::services::fw_services::donate_solar_system_lp;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Donates militia-corp Loyalty Points to a warzone system's I-Hub (`facWarMgr.DonateLPsToSolarSystem`),
/// upgrading system Tier (`1..=5`) and lowering local NPC station broker fees & industry job costs.
pub fn donate_lp_to_fw_ihub(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    lp_amount: i64,
) -> Result<(i64, u8, f64, f64), BridgeRpcError> {
    if lp_amount <= 0 {
        return Err(BridgeRpcError {
            code: "InvalidLpDonation".into(),
            message: "LP donation amount must be positive".into(),
        });
    }
    let (old_lp, new_lp, tier) = ctx.db.try_with_mut_snapshot(|snap| {
        let occ = snap.fw_system_occupations.get(&sys_id).ok_or_else(|| BridgeRpcError {
            code: "NotInWarzone".into(),
            message: format!("System {sys_id} is not in an active faction warzone"),
        })?;
        let occ_fac = occ.occupier_faction_id;
        let lp_corp = militia_lp_corp_id(&ctx.sde, occ_fac);
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
        })?;
        if ch.war_faction_id != Some(occ_fac) {
            return Err(BridgeRpcError {
                code: "NotEnlistedInOccupyingMilitia".into(),
                message: format!(
                    "Character {char_id} must be enlisted in occupying militia {occ_fac} to donate LP"
                ),
            });
        }
        let char_sys = ch.solar_system_id;
        let is_in_or_near = char_sys == sys_id
            || ctx.sde.jumps.values().any(|j| {
                (j.from_solar_system_id == char_sys && j.to_solar_system_id == sys_id)
                    || (j.from_solar_system_id == sys_id && j.to_solar_system_id == char_sys)
            });
        if !is_in_or_near {
            return Err(BridgeRpcError {
                code: "NotInOrNearSystem".into(),
                message: format!(
                    "Character in system {char_sys} is not in or near target system {sys_id}"
                ),
            });
        }
        let (lp_key, have) = if let Some(&bal) = ch.loyalty_points.get(&lp_corp) {
            (lp_corp, bal)
        } else {
            (occ_fac, ch.loyalty_points.get(&occ_fac).copied().unwrap_or(0))
        };
        if have < lp_amount {
            return Err(BridgeRpcError {
                code: "InsufficientLoyaltyPoints".into(),
                message: format!(
                    "Have {have} LP with militia corporation {lp_corp}, tried to donate {lp_amount}"
                ),
            });
        }
        ch.loyalty_points.insert(lp_key, have - lp_amount);
        let old = snap
            .fw_runtime
            .ihub_lp_donations
            .get(&sys_id)
            .copied()
            .unwrap_or(0);
        let total = old + lp_amount;
        snap.fw_runtime.ihub_lp_donations.insert(sys_id, total);
        let tier: u8 = match total {
            0..=39_999 => 0,
            40_000..=59_999 => 1,
            60_000..=89_999 => 2,
            90_000..=139_999 => 3,
            140_000..=199_999 => 4,
            _ => 5,
        };
        snap.fw_runtime.ihub_tiers.insert(sys_id, tier);
        Ok((old, total, tier))
    })?;

    donate_solar_system_lp(ctx, sys_id, old_lp, new_lp);
    let reduction = f64::from(tier) * 10.0;
    Ok((new_lp, tier, reduction, reduction))
}

fn set_corp_militia_faction(snap: &mut eve_db::WorldStateSnapshot, cid: i64, fac: Option<i64>) {
    if let Some(f) = fac {
        snap.fw_runtime.corp_war_factions.insert(cid, f);
    } else {
        snap.fw_runtime.corp_war_factions.remove(&cid);
    }
    let direct_map = snap.fw_runtime.direct_enlisted_chars.clone();
    for ch in snap.characters.values_mut() {
        if ch.corporation_id == cid {
            ch.war_faction_id = fac.or_else(|| direct_map.get(&ch.character_id).copied());
        }
    }
}

fn corp_standing_with_faction(snap: &eve_db::WorldStateSnapshot, cid: i64, fac: i64) -> f64 {
    if let Some(c) = snap
        .corp_contacts
        .get(&cid)
        .and_then(|l| l.iter().find(|c| c.contact_id == fac))
    {
        return c.standing;
    }
    if let Some(txs) = snap.standing_transactions.get(&cid) {
        let s: f64 = txs
            .iter()
            .filter(|t| (t.to_id == cid && t.from_id == fac) || (t.from_id == cid && t.to_id == fac))
            .map(|t| t.modification)
            .sum();
        if s != 0.0 {
            return s;
        }
    }
    let (mut sum, mut cnt) = (0.0, 0usize);
    if let Some(mems) = snap.corporations.get(&cid).map(|c| &c.members) {
        for mid in mems {
            if let Some(st) = snap.characters.get(mid).and_then(|ch| ch.standings.get(&fac)) {
                sum += st;
                cnt += 1;
            }
        }
    }
    if cnt > 0 { sum / (cnt as f64) } else { 0.0 }
}

fn leaderboards_summary(
    ctx: &ServerContext,
) -> (
    Vec<(i64, i64, i64, i64)>,
    Vec<(i64, i64, i64)>,
    Vec<(i64, i64, i64)>,
) {
    ctx.db.with_snapshot(|snap| {
        let mut char_rows: Vec<(i64, i64, i64, i64)> = snap
            .fw_character_stats
            .values()
            .map(|r| {
                (
                    r.character_id,
                    r.kills.yesterday,
                    r.kills.last_week,
                    r.kills.total + r.victory_points.total,
                )
            })
            .collect();
        char_rows.sort_by(|a, b| b.3.cmp(&a.3));
        let mut corp_map: HashMap<i64, (i64, i64)> = HashMap::new();
        let mut fac_map: HashMap<i64, (i64, i64)> = HashMap::new();
        for r in snap.fw_character_stats.values() {
            if let Some(ch) = snap.characters.get(&r.character_id) {
                let ce = corp_map.entry(ch.corporation_id).or_insert((0, 0));
                ce.0 += r.kills.total;
                ce.1 += r.victory_points.total;
            }
            if let Some(fid) = r.faction_id {
                let fe = fac_map.entry(fid).or_insert((0, 0));
                fe.0 += r.kills.total;
                fe.1 += r.victory_points.total;
            }
        }
        let mut corp_rows: Vec<(i64, i64, i64)> =
            corp_map.into_iter().map(|(c, (k, v))| (c, k, v)).collect();
        corp_rows.sort_by(|a, b| b.2.cmp(&a.2));
        let mut fac_rows: Vec<(i64, i64, i64)> =
            fac_map.into_iter().map(|(f, (k, v))| (f, k, v)).collect();
        fac_rows.sort_by(|a, b| b.2.cmp(&a.2));
        (char_rows, corp_rows, fac_rows)
    })
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("facWarMgr", "GetStats_TopAndAllKillsAndVPs") => {
            let (chars, corps, facs) = leaderboards_summary(ctx);
            let char_list: Vec<_> = chars
                .into_iter()
                .map(|(c, y, w, tot)| {
                    PyRep::keyval([
                        ("characterID", PyRep::Integer(c)),
                        ("yesterday", PyRep::Integer(y)),
                        ("lastWeek", PyRep::Integer(w)),
                        ("total", PyRep::Integer(tot)),
                    ])
                })
                .collect();
            let corp_list: Vec<_> = corps
                .into_iter()
                .map(|(c, k, vp)| {
                    PyRep::keyval([
                        ("corporationID", PyRep::Integer(c)),
                        ("kills", PyRep::Integer(k)),
                        ("victoryPoints", PyRep::Integer(vp)),
                    ])
                })
                .collect();
            let fac_list: Vec<_> = facs
                .into_iter()
                .map(|(f, k, vp)| {
                    PyRep::keyval([
                        ("factionID", PyRep::Integer(f)),
                        ("kills", PyRep::Integer(k)),
                        ("victoryPoints", PyRep::Integer(vp)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::keyval([
                ("characters", PyRep::List(char_list)),
                ("corporations", PyRep::List(corp_list)),
                ("factions", PyRep::List(fac_list)),
            ])))
        }
        ("facWarMgr", "GetCorporationWarFactionID") => {
            let cid = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.corporation_id)
                .unwrap_or(0);
            let fac = ctx
                .db
                .with_snapshot(|s| s.fw_runtime.corp_war_factions.get(&cid).copied());
            Some(Ok(fac.map(PyRep::Integer).unwrap_or(PyRep::None)))
        }
        ("facWarMgr", "GetFactionMilitiaCorporation") => {
            let fid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            Some(Ok(PyRep::Integer(militia_lp_corp_id(&ctx.sde, fid))))
        }
        ("facWarMgr", "JoinFactionAsCorporation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (cid, is_ceo, cur_fac) = ctx.db.with_snapshot(|snap| {
                let cid = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id))
                    .unwrap_or(0);
                let is_ceo = snap.corporations.get(&cid).is_some_and(|c| {
                    c.ceo_id == char_id
                        || (c.member_roles.get(&char_id).copied().unwrap_or(0)
                            & eve_db::corp_roles::ROLE_DIRECTOR)
                            != 0
                }) || (session.corp_role & eve_db::corp_roles::ROLE_DIRECTOR != 0);
                let cur = snap.fw_runtime.corp_war_factions.get(&cid).copied();
                (cid, is_ceo, cur)
            });
            if !is_ceo {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Only corporation CEO or Director can enlist in militia".into(),
                }));
            }
            if let Some(existing) = cur_fac {
                return Some(Err(BridgeRpcError {
                    code: "CorporationAlreadyEnlisted".into(),
                    message: format!("Corporation is already enlisted in militia {existing}"),
                }));
            }
            let fac = match args
                .iter()
                .find_map(|a| a.as_i64().filter(|&f| matches!(f, 500_001..=500_004 | 500_010 | 500_011)))
                .or_else(|| args.last().and_then(|a| a.as_i64()))
            {
                Some(f) if matches!(f, 500_001..=500_004 | 500_010 | 500_011) => f,
                Some(other) => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidFWMilitiaFaction".into(),
                        message: format!("Faction {other} is not an eligible FW militia faction"),
                    }))
                }
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgs".into(),
                        message: "Militia faction ID required".into(),
                    }))
                }
            };
            let standing = ctx.db.with_snapshot(|snap| corp_standing_with_faction(snap, cid, fac));
            if standing < 0.0 {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientStanding".into(),
                    message: format!("Corporation standing with faction {fac} too low ({standing:.2})"),
                }));
            }
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                snap.fw_corp_enlisted_on.insert(cid, now);
                set_corp_militia_faction(snap, cid, Some(fac));
            });
            session.war_faction_id = Some(fac);
            Some(Ok(PyRep::Bool(true)))
        }
        ("facWarMgr", "LeaveFactionAsCorporation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (cid, is_ceo, cur_fac, enlisted_on) = ctx.db.with_snapshot(|snap| {
                let cid = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id))
                    .unwrap_or(0);
                let is_ceo = snap.corporations.get(&cid).is_some_and(|c| {
                    c.ceo_id == char_id
                        || (c.member_roles.get(&char_id).copied().unwrap_or(0)
                            & eve_db::corp_roles::ROLE_DIRECTOR)
                            != 0
                }) || (session.corp_role & eve_db::corp_roles::ROLE_DIRECTOR != 0);
                let cur = snap.fw_runtime.corp_war_factions.get(&cid).copied();
                let enl = snap.fw_corp_enlisted_on.get(&cid).copied().unwrap_or(0);
                (cid, is_ceo, cur, enl)
            });
            if !is_ceo {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Only corporation CEO or Director can resign from militia".into(),
                }));
            }
            if cur_fac.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "CorporationNotEnlisted".into(),
                    message: "Corporation is not enlisted in any militia".into(),
                }));
            }
            let now = ctx.now_unix();
            if now < enlisted_on.saturating_add(86_400) {
                return Some(Err(BridgeRpcError {
                    code: "MilitiaCooldownActive".into(),
                    message: "Must wait 24 hours after enlisting to resign from militia".into(),
                }));
            }
            let new_char_fac = ctx.db.with_mut_snapshot(|snap| {
                snap.fw_corp_enlisted_on.remove(&cid);
                set_corp_militia_faction(snap, cid, None);
                snap.characters.get(&char_id).and_then(|c| c.war_faction_id)
            });
            session.war_faction_id = new_char_fac;
            Some(Ok(PyRep::Bool(true)))
        }
        ("facWarMgr", "JoinFactionAsAlliance") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fac = match args
                .iter()
                .find_map(|a| a.as_i64().filter(|&f| matches!(f, 500_001..=500_004 | 500_010 | 500_011)))
                .or_else(|| args.last().and_then(|a| a.as_i64()))
            {
                Some(f) if matches!(f, 500_001..=500_004 | 500_010 | 500_011) => f,
                Some(other) => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidFWMilitiaFaction".into(),
                        message: format!("Faction {other} is not an eligible FW militia faction"),
                    }))
                }
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgs".into(),
                        message: "Valid militia faction ID required".into(),
                    }))
                }
            };
            let now = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let cid = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id))
                    .unwrap_or(0);
                let aid = session
                    .alliance_id
                    .or_else(|| snap.characters.get(&char_id).and_then(|c| c.alliance_id))
                    .ok_or_else(|| BridgeRpcError {
                        code: "NotInAlliance".into(),
                        message: "Caller is not in an alliance".into(),
                    })?;
                let alliance = snap.alliances.get(&aid).cloned().ok_or_else(|| BridgeRpcError {
                    code: "AllianceNotFound".into(),
                    message: format!("Alliance {aid} not found"),
                })?;
                let is_exec = alliance.executor_corp_id == cid
                    && snap.corporations.get(&cid).is_some_and(|c| c.ceo_id == char_id);
                if !is_exec {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Only the alliance executor corporation CEO can enlist the alliance".into(),
                    });
                }
                if snap.fw_runtime.corp_war_factions.contains_key(&cid) {
                    return Err(BridgeRpcError {
                        code: "AllianceAlreadyEnlisted".into(),
                        message: "Alliance is already enlisted in Factional Warfare".into(),
                    });
                }
                let mut member_corps: Vec<i64> = alliance.member_corp_ids.iter().copied().collect();
                if !member_corps.contains(&cid) {
                    member_corps.push(cid);
                }
                for &mc in &member_corps {
                    let st = corp_standing_with_faction(snap, mc, fac);
                    if st < 0.0 {
                        return Err(BridgeRpcError {
                            code: "InsufficientStanding".into(),
                            message: format!("Member corporation {mc} standing with {fac} is {st:.2} (< 0.0)"),
                        });
                    }
                }
                for mc in member_corps {
                    snap.fw_corp_enlisted_on.insert(mc, now);
                    set_corp_militia_faction(snap, mc, Some(fac));
                }
                Ok(())
            });
            if let Err(e) = res {
                return Some(Err(e));
            }
            session.war_faction_id = Some(fac);
            Some(Ok(PyRep::Bool(true)))
        }
        ("facWarMgr", "LeaveFactionAsAlliance") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let cid = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id))
                    .unwrap_or(0);
                let aid = session
                    .alliance_id
                    .or_else(|| snap.characters.get(&char_id).and_then(|c| c.alliance_id))
                    .ok_or_else(|| BridgeRpcError {
                        code: "NotInAlliance".into(),
                        message: "Caller is not in an alliance".into(),
                    })?;
                let alliance = snap.alliances.get(&aid).cloned().ok_or_else(|| BridgeRpcError {
                    code: "AllianceNotFound".into(),
                    message: format!("Alliance {aid} not found"),
                })?;
                let is_exec = alliance.executor_corp_id == cid
                    && snap.corporations.get(&cid).is_some_and(|c| c.ceo_id == char_id);
                if !is_exec {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Only the alliance executor corporation CEO can withdraw the alliance".into(),
                    });
                }
                if !snap.fw_runtime.corp_war_factions.contains_key(&cid) {
                    return Err(BridgeRpcError {
                        code: "AllianceNotEnlisted".into(),
                        message: "Alliance is not enlisted in Factional Warfare".into(),
                    });
                }
                let enl = snap.fw_corp_enlisted_on.get(&cid).copied().unwrap_or(0);
                if now < enl.saturating_add(86_400) {
                    return Err(BridgeRpcError {
                        code: "MilitiaCooldownActive".into(),
                        message: "Must wait 24 hours after enlisting to withdraw alliance from militia".into(),
                    });
                }
                let mut member_corps: Vec<i64> = alliance.member_corp_ids.iter().copied().collect();
                if !member_corps.contains(&cid) {
                    member_corps.push(cid);
                }
                for mc in member_corps {
                    snap.fw_corp_enlisted_on.remove(&mc);
                    set_corp_militia_faction(snap, mc, None);
                }
                Ok(snap.characters.get(&char_id).and_then(|c| c.war_faction_id))
            });
            match res {
                Ok(new_fac) => {
                    session.war_faction_id = new_fac;
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("facWarMgr", "DonateLPsToSolarSystem") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .or(session.solar_system_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "No solar system specified".into(),
                }));
            };
            let amt = match args.get(1).and_then(|a| a.as_i64()) {
                Some(a) if a > 0 => a,
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidLpDonation".into(),
                        message: "LP donation amount must be positive".into(),
                    }))
                }
            };
            Some(donate_lp_to_fw_ihub(ctx, char_id, sys_id, amt).map(|(tot, tier, tax, fac)| {
                PyRep::keyval([
                    ("totalLP", PyRep::Integer(tot)),
                    ("systemTier", PyRep::Integer(tier as i64)),
                    ("tier", PyRep::Integer(tier as i64)),
                    ("taxReduction", PyRep::Real(tax)),
                    ("facilityBonus", PyRep::Real(fac)),
                ])
            }))
        }
        _ => None,
    }
}
