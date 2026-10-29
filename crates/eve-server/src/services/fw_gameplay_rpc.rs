//! `fw_gameplay_rpc` — Live `facWarMgr` MachoNet RPC Handler, Militia Corp/Alliance Enlistment, Leaderboards & I-Hub LP Donations (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fw_gameplay::{
    apply_fw_or_insurgency_structure_damage, militia_lp_corp_id, TYPE_ACCELERATION_GATE,
};
use crate::services::fw_services::{
    award_fw_kill_lp, donate_solar_system_lp, fw_opposing_faction, set_facwar_bunker_vulnerable,
    update_system_advantage, HQ_SYSTEM_AMO, HQ_SYSTEM_INTAKI, HQ_SYSTEM_MEHATOOR,
    HQ_SYSTEM_ONNAMON,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

/// Returns War HQ station services if `sys_id` is one of the 4 Empire Militia War HQ systems.
pub fn get_war_hq_services(sys_id: u32) -> Option<(&'static str, Vec<&'static str>)> {
    match sys_id {
        HQ_SYSTEM_MEHATOOR => Some((
            "24th Imperial Crusade HQ (Mehatoor)",
            vec!["MilitiaCommandHQ", "CounterInsurgencyBriefing", "MilitiaLPStore"],
        )),
        HQ_SYSTEM_ONNAMON => Some((
            "State Protectorate HQ (Onnamon)",
            vec!["MilitiaCommandHQ", "CounterInsurgencyBriefing", "MilitiaLPStore"],
        )),
        HQ_SYSTEM_INTAKI => Some((
            "Federal Defence Union HQ (Intaki)",
            vec!["MilitiaCommandHQ", "CounterInsurgencyBriefing", "MilitiaLPStore"],
        )),
        HQ_SYSTEM_AMO => Some((
            "Tribal Liberation Force HQ (Amo)",
            vec!["MilitiaCommandHQ", "CounterInsurgencyBriefing", "MilitiaLPStore"],
        )),
        _ => None,
    }
}

/// Credits FW PvP kill LP, VP, and leaderboard kills when an enlisted pilot destroys an opposing militia pilot's ship.
pub fn on_player_ship_destroyed_in_combat(
    ctx: &ServerContext,
    sys_id: u32,
    killer_char_id: i64,
    victim_char_id: i64,
    victim_ship_type_id: u32,
) {
    if killer_char_id <= 0 || killer_char_id == victim_char_id {
        return;
    }
    let is_enemy_militia_kill = ctx.db.with_snapshot(|snap| {
        if !snap.fw_system_occupations.contains_key(&sys_id) {
            return false;
        }
        let k_fac = snap.characters.get(&killer_char_id).and_then(|c| c.war_faction_id);
        let v_fac = snap.characters.get(&victim_char_id).and_then(|c| c.war_faction_id);
        match (k_fac, v_fac) {
            (Some(kf), Some(vf)) => fw_opposing_faction(kf) == Some(vf) || kf != vf,
            _ => false,
        }
    });
    if is_enemy_militia_kill {
        let hull_val = ctx
            .sde
            .types
            .get(&victim_ship_type_id)
            .map(|t| t.base_price)
            .unwrap_or(10_000_000.0)
            .max(1_000_000.0);
        let _ = award_fw_kill_lp(ctx, killer_char_id, sys_id, hull_val);
    }
}

/// Invoked from `combat_weapons_spool.rs` when a non-player ball is destroyed in space.
pub async fn on_space_entity_destroyed(
    ctx: &ServerContext,
    _sys_id: u32,
    killer_char_id: Option<i64>,
    target_ball_id: i64,
    _target_type_id: u32,
) {
    let Some(killer) = killer_char_id else {
        return;
    };
    let is_supply = ctx
        .db
        .with_snapshot(|s| s.fw_runtime.supply_sites.contains_key(&target_ball_id));
    if is_supply {
        let _ = apply_fw_or_insurgency_structure_damage(ctx, killer, target_ball_id, 1.0e9).await;
    }
}

/// Defends a friendly Supply Cache or Supply Depot in an FW warzone system,
/// shifting system Advantage and emitting `OnSolarsystemAdvantageStateUpdated_Local`.
pub fn resolve_friendly_supply_site_defense(
    ctx: &ServerContext,
    char_id: i64,
    structure_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let (sys_id, site_kind, site_fac) = ctx
        .db
        .with_snapshot(|s| s.fw_runtime.supply_sites.get(&structure_id).cloned())
        .ok_or_else(|| BridgeRpcError {
            code: "StructureNotFound".into(),
            message: format!("Supply site {structure_id} not found"),
        })?;
    let char_fac = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.war_faction_id))
        .ok_or_else(|| BridgeRpcError {
            code: "NotEnlistedInMilitia".into(),
            message: "Only militia-enlisted pilots can defend friendly Supply Caches/Depots".into(),
        })?;
    if char_fac != site_fac {
        return Err(BridgeRpcError {
            code: "CannotDefendEnemyStructure".into(),
            message: "Cannot defend an enemy militia Supply Cache/Depot".into(),
        });
    }
    let is_depot = site_kind == "SupplyDepot";
    let adv_delta = if is_depot { 5.0 } else { 2.0 };
    let occ_fac = ctx
        .db
        .with_snapshot(|s| s.fw_system_occupations.get(&sys_id).map(|o| o.occupier_faction_id))
        .unwrap_or(site_fac);
    let (occ_d, att_d) = if site_fac == occ_fac { (adv_delta, 0.0) } else { (0.0, adv_delta) };
    let (occ_adv, att_adv) = update_system_advantage(ctx, sys_id, occ_d, att_d);
    let (friendly_adv, enemy_adv) = if site_fac == occ_fac { (occ_adv, att_adv) } else { (att_adv, occ_adv) };
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            let max_hp = if is_depot { 3_000.0 } else { 1_500.0 };
            st.shield_hp = max_hp;
            st.armor_hp = max_hp;
            st.hull_hp = max_hp;
        }
    });
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("defended", PyRep::Bool(true)),
        ("siteKind", PyRep::String(site_kind)),
        ("friendlyAdvantage", PyRep::Real(friendly_adv)),
        ("enemyAdvantage", PyRep::Real(enemy_adv)),
    ]))
}

/// Regenerates an unattacked vulnerable FW I-Hub after 2 hours, lowering system contested % back to `80%`.
pub fn regenerate_unattacked_fw_ihub(ctx: &ServerContext, sys_id: u32) -> f64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.ihub_vulnerable.insert(sys_id, false);
        if let Some(occ) = snap.fw_system_occupations.get_mut(&sys_id) {
            let reduced_vp = (occ.victory_points_threshold * 80) / 100;
            occ.victory_points = reduced_vp;
            snap.fw_runtime.system_victory_points.insert(sys_id, reduced_vp);
        }
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.fw_contested_pct = 80.0;
        }
    });
    set_facwar_bunker_vulnerable(ctx, sys_id, 0, false);
    80.0
}

/// Spawns a combat-capable militia NPC wave ship (`ExtendedNpcEntity` + `CombatState`) inside a Battlefield capture zone.
pub async fn spawn_battlefield_wave_npc(
    ctx: &ServerContext,
    sys_id: u32,
    bf_id: i64,
    nid: i64,
    ntid: u32,
    fac_id: i64,
    npos: DVec3,
) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if sys_handle.get_ball_snapshot(bf_id).await.ok().flatten().is_none() {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: bf_id,
                type_id: TYPE_ACCELERATION_GATE,
                position: npos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e8,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }
    let mut cs = ctx.build_type_combat_state(ntid);
    if cs.max_structure_hp <= 0.0 {
        cs.max_shield_hp = 2_500.0;
        cs.shield_hp = 2_500.0;
        cs.max_armor_hp = 2_500.0;
        cs.armor_hp = 2_500.0;
        cs.max_structure_hp = 3_000.0;
        cs.structure_hp = 3_000.0;
    }
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: nid,
                type_id: ntid,
                position: npos,
                velocity: DVec3::new(120.0, 0.0, 0.0),
                max_velocity: 280.0,
                inertia: 1.0,
                mass: 1.2e7,
                radius: 120.0,
                speed_fraction: 0.5,
                mode: MovementMode::Orbit {
                    target_id: bf_id,
                    range: 5_000.0,
                },
            },
            cs,
        )
        .await;
    let entity = crate::services::combat_crimewatch::ExtendedNpcEntity {
        ball_id: nid,
        type_id: ntid,
        faction_id: fac_id as u32,
        archetype: "BattlefieldMilitiaWave".into(),
        solar_system_id: sys_id,
        spawn_group_id: bf_id as u32,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: 150_000.0,
        optimal_range: 18_000.0,
        orbit_range: 5_000.0,
        damage_vector: eve_destiny::DamageVector {
            em: 25.0,
            explosive: 25.0,
            kinetic: 25.0,
            thermal: 25.0,
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
        spawn_position: npos,
        sleeper_wh_class: 0,
        custom_cargo_loot: None,
    };
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.npc_entities.insert(nid, entity);
    });
}

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
