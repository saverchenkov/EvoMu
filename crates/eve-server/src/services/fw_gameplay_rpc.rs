//! `fw_gameplay_rpc` — Live `facWarMgr` MachoNet RPC Handler, Militia Corp/Alliance Enlistment, Leaderboards & I-Hub LP Donations (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fw_gameplay::{
    apply_fw_or_insurgency_structure_damage, militia_lp_corp_id, TYPE_ACCELERATION_GATE,
};
use crate::services::fw_services::{
    award_fw_kill_lp_split, donate_solar_system_lp,
    set_facwar_bunker_vulnerable, update_system_advantage, HQ_SYSTEM_AMO, HQ_SYSTEM_INTAKI,
    HQ_SYSTEM_MEHATOOR, HQ_SYSTEM_ONNAMON,
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

/// Credits FW PvP kill LP and leaderboard kills when opposing-militia pilots destroy a player ship (`feat:fw.fw-lp`).
/// Uses net loss (`hull - insurance + fit + cargo`), splits equally across involved militia attackers,
/// pays `0 LP` for empty Capsules (`670`), and never pushes solar system contested VP.
pub fn on_player_ship_destroyed_in_combat_detailed(
    ctx: &ServerContext,
    sys_id: u32,
    killer_char_id: i64,
    victim_char_id: i64,
    victim_ship_type_id: u32,
    net_loss_isk: f64,
    killmail_id: i64,
) {
    if killer_char_id <= 0 || killer_char_id == victim_char_id {
        return;
    }
    let mut attackers: Vec<i64> = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.detailed_killmails
            .get(&killmail_id)
            .map(|dkm| dkm.attackers.iter().map(|a| a.character_id).collect())
            .unwrap_or_default()
    });
    if !attackers.contains(&killer_char_id) {
        attackers.push(killer_char_id);
    }
    let _ = award_fw_kill_lp_split(
        ctx,
        &attackers,
        victim_char_id,
        sys_id,
        victim_ship_type_id,
        net_loss_isk,
    );
}

pub fn on_player_ship_destroyed_in_combat(
    ctx: &ServerContext,
    sys_id: u32,
    killer_char_id: i64,
    victim_char_id: i64,
    victim_ship_type_id: u32,
) {
    let hull_isk = if victim_ship_type_id == 670 {
        0.0
    } else {
        ctx.sde
            .types
            .get(&victim_ship_type_id)
            .map(|t| t.base_price)
            .filter(|&p| p > 0.0)
            .unwrap_or(1_000_000.0)
    };
    on_player_ship_destroyed_in_combat_detailed(
        ctx,
        sys_id,
        killer_char_id,
        victim_char_id,
        victim_ship_type_id,
        hull_isk,
        0,
    );
}

/// Invoked from `combat_weapons_spool.rs` when a non-player ball is destroyed in space.
pub async fn on_space_entity_destroyed(
    ctx: &ServerContext,
    _sys_id: u32,
    killer_char_id: Option<i64>,
    target_ball_id: i64,
    _target_type_id: u32,
) {
    if target_ball_id > 500_000_000 {
        let complex_gid = target_ball_id - 500_000_000;
        ctx.db.with_mut_snapshot(|s| {
            if s.fw_runtime.fw_complexes.contains_key(&complex_gid) {
                s.fw_runtime.complex_guards_cleared.insert(complex_gid, true);
            }
        });
    }
    let Some(kid) = killer_char_id else {
        return;
    };
    let (is_supply, killer) = ctx.db.with_snapshot(|s| {
        let resolved_cid = s
            .characters
            .get(&kid)
            .map(|c| c.character_id)
            .or_else(|| {
                s.characters
                    .values()
                    .find(|c| c.active_ship_id == kid)
                    .map(|c| c.character_id)
            })
            .unwrap_or(kid);
        (s.fw_runtime.supply_sites.contains_key(&target_ball_id), resolved_cid)
    });
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
            client_error: None,
        })?;
    let char_fac = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.war_faction_id))
        .ok_or_else(|| BridgeRpcError {
            code: "NotEnlistedInMilitia".into(),
            message: "Only militia-enlisted pilots can defend friendly Supply Caches/Depots".into(),
            client_error: None,
        })?;
    if char_fac != site_fac {
        return Err(BridgeRpcError {
            code: "CannotDefendEnemyStructure".into(),
            message: "Cannot defend an enemy militia Supply Cache/Depot".into(),
            client_error: None,
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
            client_error: None,
        });
    }
    let (old_lp, new_lp, tier) = ctx.db.try_with_mut_snapshot(|snap| {
        let occ = snap.fw_system_occupations.get(&sys_id).ok_or_else(|| BridgeRpcError {
            code: "NotInWarzone".into(),
            message: format!("System {sys_id} is not in an active faction warzone"),
            client_error: None,
        })?;
        let occ_fac = occ.occupier_faction_id;
        let lp_corp = militia_lp_corp_id(&ctx.sde, occ_fac);
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
            client_error: None,
        })?;
        if ch.war_faction_id != Some(occ_fac) {
            return Err(BridgeRpcError {
                code: "NotEnlistedInOccupyingMilitia".into(),
                message: format!(
                    "Character {char_id} must be enlisted in occupying militia {occ_fac} to donate LP"
                ),
                client_error: None,
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
                client_error: None,
            });
        }
        const MAX_IHUB_LP: i64 = 200_000;
        let old = snap
            .fw_runtime
            .ihub_lp_donations
            .get(&sys_id)
            .copied()
            .unwrap_or(0);
        let remaining_cap = MAX_IHUB_LP.saturating_sub(old);
        if remaining_cap <= 0 {
            return Err(BridgeRpcError {
                code: "IhubMaxTierReached".into(),
                message: format!(
                    "Solar system {sys_id} iHub has reached maximum Tier 5 LP capacity ({MAX_IHUB_LP})"
                ),
                client_error: None,
            });
        }
        let effective_lp = lp_amount.min(remaining_cap);
        let corp_bal = ch.loyalty_points.get(&lp_corp).copied().unwrap_or(0).max(0);
        let fac_bal = if lp_corp != occ_fac {
            ch.loyalty_points.get(&occ_fac).copied().unwrap_or(0).max(0)
        } else {
            0
        };
        let have = corp_bal.saturating_add(fac_bal);
        if have < effective_lp {
            return Err(BridgeRpcError {
                code: "InsufficientLoyaltyPoints".into(),
                message: format!(
                    "Have {have} LP with militia corporation {lp_corp}, tried to donate {effective_lp}"
                ),
                client_error: None,
            });
        }
        let from_corp = effective_lp.min(corp_bal);
        let from_fac = effective_lp - from_corp;
        if from_corp > 0 {
            ch.loyalty_points.insert(lp_corp, corp_bal - from_corp);
        }
        if from_fac > 0 {
            ch.loyalty_points.insert(occ_fac, fac_bal - from_fac);
        }
        let total = old + effective_lp;
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

fn set_corp_militia_faction(
    snap: &mut eve_db::WorldStateSnapshot,
    cid: i64,
    fac: Option<i64>,
    now_unix: u64,
) -> Vec<crate::services::fw_enlistment::FwMemberMutation> {
    crate::services::fw_enlistment::apply_corp_militia_faction_in_snap(snap, cid, fac, now_unix)
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

const FW_STAT_KEYS: [&str; 6] = ["killsY", "killsLW", "killsTotal", "vpY", "vpLW", "vpTotal"];

#[derive(Debug, Clone, Copy, Default)]
struct SixBucketStats {
    vals: [i64; 6],
}

impl SixBucketStats {
    fn add_event(&mut self, age_secs: u64, kills: i64, vps: i64) {
        if age_secs < 86_400 {
            self.vals[0] += kills;
            self.vals[3] += vps;
        }
        if age_secs < 7 * 86_400 {
            self.vals[1] += kills;
            self.vals[4] += vps;
        }
        self.vals[2] += kills;
        self.vals[5] += vps;
    }

    fn to_py_dict(self) -> PyRep {
        PyRep::Dict(
            FW_STAT_KEYS
                .iter()
                .enumerate()
                .map(|(i, &k)| (PyRep::String(k.into()), PyRep::Integer(self.vals[i])))
                .collect(),
        )
    }
}

fn build_entity_subtables(id_key: &str, map: &HashMap<i64, SixBucketStats>) -> (PyRep, SixBucketStats, SixBucketStats) {
    let mut top = SixBucketStats::default();
    let mut all = SixBucketStats::default();
    for s in map.values() {
        for i in 0..6 {
            if s.vals[i] > top.vals[i] {
                top.vals[i] = s.vals[i];
            }
            all.vals[i] += s.vals[i];
        }
    }
    let mut sub = Vec::with_capacity(6);
    for (idx, &k) in FW_STAT_KEYS.iter().enumerate() {
        let mut rows: Vec<(i64, i64)> = map
            .iter()
            .filter_map(|(&id, s)| (s.vals[idx] > 0).then_some((id, s.vals[idx])))
            .collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        rows.truncate(100);
        let list = rows
            .into_iter()
            .map(|(id, amt)| {
                PyRep::keyval([
                    (id_key, PyRep::Integer(id)),
                    ("amount", PyRep::Integer(amt)),
                ])
            })
            .collect();
        sub.push((PyRep::String(k.into()), PyRep::List(list)));
    }
    (PyRep::Dict(sub), top, all)
}

fn compute_fw_leaderboards(
    ctx: &ServerContext,
) -> (
    HashMap<i64, SixBucketStats>,
    HashMap<i64, SixBucketStats>,
    HashMap<i64, SixBucketStats>,
    HashMap<i64, SixBucketStats>,
) {
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let mut chars: HashMap<i64, SixBucketStats> = HashMap::new();
        let mut corps: HashMap<i64, SixBucketStats> = HashMap::new();
        let mut facs: HashMap<i64, SixBucketStats> = HashMap::new();
        let mut allys: HashMap<i64, SixBucketStats> = HashMap::new();

        // Seed lifetime totals from folded historical events (> 7 days old)
        for (&cid, &(k, vp)) in &snap.fw_runtime.historical_char_totals {
            let s = chars.entry(cid).or_default();
            s.vals[2] += k;
            s.vals[5] += vp;
        }
        for (&cid, &(k, vp)) in &snap.fw_runtime.historical_corp_totals {
            let s = corps.entry(cid).or_default();
            s.vals[2] += k;
            s.vals[5] += vp;
        }
        for (&fid, &(k, vp)) in &snap.fw_runtime.historical_fac_totals {
            let s = facs.entry(fid).or_default();
            s.vals[2] += k;
            s.vals[5] += vp;
        }
        for (&aid, &(k, vp)) in &snap.fw_runtime.historical_ally_totals {
            let s = allys.entry(aid).or_default();
            s.vals[2] += k;
            s.vals[5] += vp;
        }

        if !snap.fw_runtime.stat_events.is_empty() || !snap.fw_runtime.historical_char_totals.is_empty() {
            for ev in &snap.fw_runtime.stat_events {
                let age = now.saturating_sub(ev.timestamp_unix);
                chars.entry(ev.character_id).or_default().add_event(age, ev.kills, ev.victory_points);
                if ev.corporation_id > 0 {
                    corps.entry(ev.corporation_id).or_default().add_event(age, ev.kills, ev.victory_points);
                }
                if ev.faction_id > 0 {
                    facs.entry(ev.faction_id).or_default().add_event(age, ev.kills, ev.victory_points);
                }
                if let Some(aid) = ev.alliance_id.filter(|&a| a > 0) {
                    allys.entry(aid).or_default().add_event(age, ev.kills, ev.victory_points);
                }
            }
        } else {
            for r in snap.fw_character_stats.values() {
                let b = SixBucketStats {
                    vals: [
                        r.kills.yesterday,
                        r.kills.last_week,
                        r.kills.total,
                        r.victory_points.yesterday,
                        r.victory_points.last_week,
                        r.victory_points.total,
                    ],
                };
                chars.insert(r.character_id, b);
                if let Some(ch) = snap.characters.get(&r.character_id) {
                    if ch.corporation_id > 0 {
                        let ce = corps.entry(ch.corporation_id).or_default();
                        for i in 0..6 {
                            ce.vals[i] += b.vals[i];
                        }
                    }
                    if let Some(aid) = ch.alliance_id.filter(|&a| a > 0) {
                        let ae = allys.entry(aid).or_default();
                        for i in 0..6 {
                            ae.vals[i] += b.vals[i];
                        }
                    }
                }
                if let Some(fid) = r.faction_id.filter(|&f| f > 0) {
                    let fe = facs.entry(fid).or_default();
                    for i in 0..6 {
                        fe.vals[i] += b.vals[i];
                    }
                }
            }
        }
        (chars, corps, facs, allys)
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
            Some(handle_fw_stats_top_and_all(ctx))
        }
        ("facWarMgr", "GetStats_Character") => {
            Some(handle_fw_stats_character(ctx, session, args))
        }
        ("facWarMgr", "GetStats_Corp") => Some(handle_fw_stats_corp(ctx, session, args)),
        ("facWarMgr", "GetStats_Alliance") => Some(handle_fw_stats_alliance(ctx, session, args)),
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
            let now_unix = ctx.now_unix();
            let (cid, is_ceo, cur_fac) = ctx.db.with_snapshot(|snap| {
                let cid = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id))
                    .unwrap_or(0);
                let (corp_ceo, corp_roles) =
                    snap.effective_corp_roles_at(cid, char_id, None, session.corp_role, now_unix);
                let is_ceo = corp_ceo
                    || (corp_roles & eve_db::corp_roles::ROLE_DIRECTOR) != 0;
                let cur = snap.fw_runtime.corp_war_factions.get(&cid).copied();
                (cid, is_ceo, cur)
            });
            if !is_ceo {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Only corporation CEO or Director can enlist in militia".into(),
                    client_error: None,
                }));
            }
            if let Some(existing) = cur_fac {
                return Some(Err(BridgeRpcError {
                    code: "CorporationAlreadyEnlisted".into(),
                    message: format!("Corporation is already enlisted in militia {existing}"),
                    client_error: None,
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
                        client_error: None,
                    }))
                }
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgs".into(),
                        message: "Militia faction ID required".into(),
                        client_error: None,
                    }))
                }
            };
            let standing = ctx.db.with_snapshot(|snap| corp_standing_with_faction(snap, cid, fac));
            if standing < 0.0 {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientStanding".into(),
                    message: format!("Corporation standing with faction {fac} too low ({standing:.2})"),
                    client_error: None,
                }));
            }
            let now = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let ally_opt = session
                    .alliance_id
                    .or_else(|| snap.characters.get(&char_id).and_then(|c| c.alliance_id))
                    .and_then(|aid| snap.alliances.get(&aid));
                if let Some(ally) = ally_opt {
                    if snap.fw_runtime.corp_war_factions.contains_key(&ally.executor_corp_id)
                        || ally.member_corp_ids.iter().any(|&mc| {
                            mc != cid && snap.fw_runtime.corp_war_factions.contains_key(&mc)
                        })
                    {
                        return Err(BridgeRpcError {
                            code: "AllianceAlreadyEnlisted".into(),
                            message: "Alliance is already enlisted in Factional Warfare".into(),
                            client_error: None,
                        });
                    }
                }
                if snap
                    .fw_corp_enlisted_on
                    .get(&cid)
                    .copied()
                    .is_some_and(|enl| now < enl.saturating_add(86_400))
                    || crate::services::fw_enlistment::is_corp_leave_cooldown_active_in_snap(snap, cid, now)
                {
                    return Err(BridgeRpcError {
                        code: "MilitiaCooldownActive".into(),
                        message: "Corporation is on active 24-hour militia cooldown".into(),
                        client_error: None,
                    });
                }
                snap.fw_corp_enlisted_on.insert(cid, now);
                let changes = set_corp_militia_faction(snap, cid, Some(fac), now);
                Ok((snap.characters.get(&char_id).and_then(|c| c.war_faction_id), changes))
            });
            match res {
                Ok((new_fac, changes)) => {
                    session.war_faction_id = new_fac;
                    crate::services::fw_enlistment::sync_corp_militia_members_after_change(
                        ctx,
                        session,
                        Some(fac),
                        &changes,
                    )
                    .await;
                    crate::services::notify_emitters_character_services::emit_campaign_enrollment_changed(ctx, char_id);
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("facWarMgr", "LeaveFactionAsCorporation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let (cid, is_ceo, cur_fac, enlisted_on, alliance_block) = ctx.db.with_snapshot(|snap| {
                let cid = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id))
                    .unwrap_or(0);
                let (corp_ceo, corp_roles) =
                    snap.effective_corp_roles_at(cid, char_id, None, session.corp_role, now_unix);
                let is_ceo = corp_ceo
                    || (corp_roles & eve_db::corp_roles::ROLE_DIRECTOR) != 0;
                let cur = snap.fw_runtime.corp_war_factions.get(&cid).copied();
                let enl = snap.fw_corp_enlisted_on.get(&cid).copied().unwrap_or(0);
                let aid = session
                    .alliance_id
                    .or_else(|| snap.characters.get(&char_id).and_then(|c| c.alliance_id))
                    .or_else(|| snap.corporations.get(&cid).and_then(|c| c.alliance_id));
                let ally_block = aid
                    .and_then(|a| snap.alliances.get(&a))
                    .is_some_and(|ally| {
                        let any_other_enlisted = ally
                            .member_corp_ids
                            .iter()
                            .any(|&mc| mc != cid && snap.fw_runtime.corp_war_factions.contains_key(&mc));
                        let exec_enlisted = snap
                            .fw_runtime
                            .corp_war_factions
                            .contains_key(&ally.executor_corp_id);
                        if ally.executor_corp_id == cid {
                            any_other_enlisted
                        } else {
                            exec_enlisted || any_other_enlisted
                        }
                    });
                (cid, is_ceo, cur, enl, ally_block)
            });
            if !is_ceo {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Only corporation CEO or Director can resign from militia".into(),
                    client_error: None,
                }));
            }
            if alliance_block {
                return Some(Err(BridgeRpcError {
                    code: "AllianceEnlisted".into(),
                    message: "Cannot resign corporation individually while alliance is enlisted; use LeaveFactionAsAlliance or leave alliance".into(),
                    client_error: None,
                }));
            }
            if cur_fac.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "CorporationNotEnlisted".into(),
                    message: "Corporation is not enlisted in any militia".into(),
                    client_error: None,
                }));
            }
            let now = ctx.now_unix();
            if now < enlisted_on.saturating_add(86_400) {
                return Some(Err(BridgeRpcError {
                    code: "MilitiaCooldownActive".into(),
                    message: "Must wait 24 hours after enlisting to resign from militia".into(),
                    client_error: None,
                }));
            }
            let (new_char_fac, changes) = ctx.db.with_mut_snapshot(|snap| {
                crate::services::fw_enlistment::record_corp_leave_cooldown_in_snap(
                    snap,
                    cid,
                    now.saturating_add(86_400),
                );
                snap.fw_corp_enlisted_on.remove(&cid);
                let changes = set_corp_militia_faction(snap, cid, None, now);
                (snap.characters.get(&char_id).and_then(|c| c.war_faction_id), changes)
            });
            session.war_faction_id = new_char_fac;
            crate::services::fw_enlistment::sync_corp_militia_members_after_change(
                ctx, session, None, &changes,
            )
            .await;
            crate::services::notify_emitters_character_services::emit_campaign_enrollment_changed(ctx, char_id);
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
                        client_error: None,
                    }))
                }
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgs".into(),
                        message: "Valid militia faction ID required".into(),
                        client_error: None,
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
                        client_error: None,
                    })?;
                let alliance = snap.alliances.get(&aid).cloned().ok_or_else(|| BridgeRpcError {
                    code: "AllianceNotFound".into(),
                    message: format!("Alliance {aid} not found"),
                    client_error: None,
                })?;
                let is_exec = alliance.executor_corp_id == cid
                    && snap.corporations.get(&cid).is_some_and(|c| c.ceo_id == char_id);
                if !is_exec {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Only the alliance executor corporation CEO can enlist the alliance".into(),
                        client_error: None,
                    });
                }
                let mut member_corps: Vec<i64> = alliance.member_corp_ids.iter().copied().collect();
                if !member_corps.contains(&cid) {
                    member_corps.push(cid);
                }
                for &mc in &member_corps {
                    if let Some(&existing) = snap.fw_runtime.corp_war_factions.get(&mc) {
                        return Err(BridgeRpcError {
                            code: if mc == cid {
                                "AllianceAlreadyEnlisted"
                            } else {
                                "CorporationAlreadyEnlisted"
                            }
                            .into(),
                            message: format!("Corporation {mc} is already enlisted in militia {existing}"),
                            client_error: None,
                        });
                    }
                    if snap
                        .fw_corp_enlisted_on
                        .get(&mc)
                        .copied()
                        .is_some_and(|enl| now < enl.saturating_add(86_400))
                        || crate::services::fw_enlistment::is_corp_leave_cooldown_active_in_snap(snap, mc, now)
                    {
                        return Err(BridgeRpcError {
                            code: "MilitiaCooldownActive".into(),
                            message: format!("Member corporation {mc} is on active 24-hour militia cooldown"),
                            client_error: None,
                        });
                    }
                    let st = corp_standing_with_faction(snap, mc, fac);
                    if st < 0.0 {
                        return Err(BridgeRpcError {
                            code: "InsufficientStanding".into(),
                            message: format!("Member corporation {mc} standing with {fac} is {st:.2} (< 0.0)"),
                            client_error: None,
                        });
                    }
                }
                let mut changes = Vec::new();
                for mc in member_corps {
                    snap.fw_corp_enlisted_on.insert(mc, now);
                    changes.extend(set_corp_militia_faction(snap, mc, Some(fac), now));
                }
                Ok((snap.characters.get(&char_id).and_then(|c| c.war_faction_id), changes))
            });
            match res {
                Ok((new_fac, changes)) => {
                    session.war_faction_id = new_fac;
                    crate::services::fw_enlistment::sync_corp_militia_members_after_change(
                        ctx,
                        session,
                        Some(fac),
                        &changes,
                    )
                    .await;
                    crate::services::notify_emitters_character_services::emit_campaign_enrollment_changed(ctx, char_id);
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
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
                        client_error: None,
                    })?;
                let alliance = snap.alliances.get(&aid).cloned().ok_or_else(|| BridgeRpcError {
                    code: "AllianceNotFound".into(),
                    message: format!("Alliance {aid} not found"),
                    client_error: None,
                })?;
                let is_exec = alliance.executor_corp_id == cid
                    && snap.corporations.get(&cid).is_some_and(|c| c.ceo_id == char_id);
                if !is_exec {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Only the alliance executor corporation CEO can withdraw the alliance".into(),
                        client_error: None,
                    });
                }
                let mut member_corps: Vec<i64> = alliance.member_corp_ids.iter().copied().collect();
                if !member_corps.contains(&cid) {
                    member_corps.push(cid);
                }
                if !member_corps
                    .iter()
                    .any(|mc| snap.fw_runtime.corp_war_factions.contains_key(mc))
                {
                    return Err(BridgeRpcError {
                        code: "AllianceNotEnlisted".into(),
                        message: "Alliance is not enlisted in Factional Warfare".into(),
                        client_error: None,
                    });
                }
                let enl = snap.fw_corp_enlisted_on.get(&cid).copied().unwrap_or(0);
                if now < enl.saturating_add(86_400) {
                    return Err(BridgeRpcError {
                        code: "MilitiaCooldownActive".into(),
                        message: "Must wait 24 hours after enlisting to withdraw alliance from militia".into(),
                        client_error: None,
                    });
                }
                let mut changes = Vec::new();
                for mc in member_corps {
                    crate::services::fw_enlistment::record_corp_leave_cooldown_in_snap(
                        snap,
                        mc,
                        now.saturating_add(86_400),
                    );
                    snap.fw_corp_enlisted_on.remove(&mc);
                    changes.extend(set_corp_militia_faction(snap, mc, None, now));
                }
                Ok((snap.characters.get(&char_id).and_then(|c| c.war_faction_id), changes))
            });
            match res {
                Ok((new_fac, changes)) => {
                    session.war_faction_id = new_fac;
                    crate::services::fw_enlistment::sync_corp_militia_members_after_change(
                        ctx, session, None, &changes,
                    )
                    .await;
                    crate::services::notify_emitters_character_services::emit_campaign_enrollment_changed(ctx, char_id);
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("facWarMgr", "DonateLPsToSolarSystem") => {
            Some(handle_donate_lps_to_solar_system(ctx, session, args))
        }
        ("facWarMgr", "CheckForRankChange") => {
            let cid = session.character_id.unwrap_or(0);
            let rank = ctx.db.with_snapshot(|s| {
                s.fw_character_stats
                    .get(&cid)
                    .map(|r| r.current_rank as i64)
                    .unwrap_or(1)
            });
            Some(Ok(PyRep::Integer(rank)))
        }
        ("facWarMgr", "GetAllianceWarFactionID") => {
            let aid = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.alliance_id)
                .unwrap_or(0);
            let fac = ctx.db.with_snapshot(|s| {
                let exec_corp = s.alliances.get(&aid).map(|a| a.executor_corp_id)?;
                s.fw_runtime.corp_war_factions.get(&exec_corp).copied()
            });
            Some(Ok(fac.map(PyRep::Integer).unwrap_or(PyRep::None)))
        }
        ("facWarMgr", "GetCharacterRankInfo") | ("facWarMgr", "GetMyCharacterRankInfo") => {
            let cid = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.character_id)
                .unwrap_or(0);
            let now_ft = ctx.now_blueos_filetime_ticks();
            let (fac, cur_rank, high_rank) = ctx.db.with_snapshot(|s| {
                let st = s.fw_character_stats.get(&cid);
                let fac = st
                    .and_then(|r| r.faction_id)
                    .or_else(|| s.characters.get(&cid).and_then(|c| c.war_faction_id))
                    .or(session.war_faction_id)
                    .unwrap_or(500_001);
                let cur = st.map(|r| r.current_rank as i64).unwrap_or(1);
                let high = st.map(|r| r.highest_rank as i64).unwrap_or(cur);
                (fac, cur, high)
            });
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(cid)),
                ("factionID", PyRep::Integer(fac)),
                ("currentRank", PyRep::Integer(cur_rank)),
                ("highestRank", PyRep::Integer(high_rank)),
                ("lastModified", PyRep::Integer(now_ft)),
            ])))
        }
        ("facWarMgr", "GetMyCharacterRankOverview") => {
            let cid = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.character_id)
                .unwrap_or(0);
            let now_ft = ctx.now_blueos_filetime_ticks();
            let (fac, cur_rank, high_rank) = ctx.db.with_snapshot(|s| {
                let st = s.fw_character_stats.get(&cid);
                let fac = st
                    .and_then(|r| r.faction_id)
                    .or_else(|| s.characters.get(&cid).and_then(|c| c.war_faction_id))
                    .or(session.war_faction_id)
                    .unwrap_or(500_001);
                let cur = st.map(|r| r.current_rank as i64).unwrap_or(1);
                let high = st.map(|r| r.highest_rank as i64).unwrap_or(cur);
                (fac, cur, high)
            });
            Some(Ok(PyRep::Rowset {
                columns: vec![
                    "factionID".into(),
                    "currentRank".into(),
                    "highestRank".into(),
                    "lastModified".into(),
                ],
                rows: vec![vec![
                    PyRep::Integer(fac),
                    PyRep::Integer(cur_rank),
                    PyRep::Integer(high_rank),
                    PyRep::Integer(now_ft),
                ]],
            }))
        }
        ("facWarMgr", "GetControlBunkerIDInSystem") => {
            let Some(sys_id) = session.solar_system_id.or_else(|| {
                session.character_id.and_then(|cid| {
                    ctx.db.with_snapshot(|s| s.characters.get(&cid).map(|c| c.solar_system_id))
                })
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSolarSystem".into(),
                    message: "Active solar system required for GetControlBunkerIDInSystem".into(),
                    client_error: None,
                }));
            };
            let bunker_opt = ctx.db.with_snapshot(|s| {
                s.structures
                    .values()
                    .find(|st| {
                        st.solar_system_id == sys_id
                            && st.type_id == crate::services::fw_gameplay::TYPE_FW_IHUB
                            && st.state != "Destroyed"
                    })
                    .map(|st| st.structure_id)
                    .or_else(|| {
                        s.items
                            .values()
                            .find(|it| {
                                it.type_id == crate::services::fw_gameplay::TYPE_FW_IHUB
                                    && it.location_id == i64::from(sys_id)
                            })
                            .map(|it| it.item_id)
                    })
                    .or_else(|| {
                        s.fw_system_occupations
                            .contains_key(&sys_id)
                            .then_some(1_000_000_000 + sys_id as i64)
                    })
            });
            Some(Ok(bunker_opt.map(PyRep::Integer).unwrap_or(PyRep::None)))
        }
        ("facWarMgr", "GetCorpFactionalWarStatus") => {
            let corp_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.corporation_id)
                .unwrap_or(0);
            let now_ft = ctx.now_blueos_filetime_ticks();
            let (fac_opt, start_ft) = ctx.db.with_snapshot(|s| {
                let fac = s
                    .fw_runtime
                    .corp_war_factions
                    .get(&corp_id)
                    .copied()
                    .or_else(|| {
                        u32::try_from(corp_id).ok().and_then(|npc_cid| {
                            ctx.sde
                                .factions
                                .values()
                                .find(|f| f.militia_corporation_id == Some(npc_cid as i64))
                                .map(|f| i64::from(f.faction_id))
                        })
                    });
                let enl = s
                    .fw_corp_enlisted_on
                    .get(&corp_id)
                    .copied()
                    .map(crate::services::fw_enlistment::unix_to_filetime)
                    .unwrap_or(now_ft);
                (fac, enl)
            });
            if let Some(fac) = fac_opt {
                Some(Ok(PyRep::keyval([
                    ("factionID", PyRep::Integer(fac)),
                    ("startDate", PyRep::Tuple(vec![PyRep::Integer(start_ft)])),
                    ("status", PyRep::Integer(2)),
                ])))
            } else {
                Some(Ok(PyRep::keyval([
                    ("factionID", PyRep::None),
                    ("startDate", PyRep::None),
                    ("status", PyRep::Integer(0)),
                ])))
            }
        }
        ("facWarMgr", "GetFactionCorporations") => {
            let fac_filter = args.first().and_then(|a| a.as_i64());
            let dict = ctx.db.with_snapshot(|s| {
                let mut map: HashMap<i64, i64> = s
                    .fw_runtime
                    .corp_war_factions
                    .iter()
                    .filter(|(_, &f)| fac_filter.is_none_or(|ff| ff == f))
                    .map(|(&cid, &f)| (cid, f))
                    .collect();
                for f in ctx.sde.factions.values() {
                    let fid = i64::from(f.faction_id);
                    if fac_filter.is_none_or(|ff| ff == fid) {
                        if let Some(mc) = f.militia_corporation_id {
                            map.entry(i64::from(mc)).or_insert(fid);
                        }
                    }
                }
                let entries: Vec<(PyRep, PyRep)> = map
                    .into_iter()
                    .map(|(cid, f)| (PyRep::Integer(cid), PyRep::Integer(f)))
                    .collect();
                PyRep::Dict(entries)
            });
            Some(Ok(dict))
        }
        ("facWarMgr", "GetSolarSystemLPs") => {
            let Some(sys_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .filter(|&v| v > 0)
                .or(session.solar_system_id)
                .or_else(|| {
                    session.character_id.and_then(|cid| {
                        ctx.db.with_snapshot(|s| s.characters.get(&cid).map(|c| c.solar_system_id))
                    })
                })
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSolarSystem".into(),
                    message: "Solar system ID required for GetSolarSystemLPs".into(),
                    client_error: None,
                }));
            };
            let lp = ctx.db.with_snapshot(|s| {
                s.fw_runtime.ihub_lp_donations.get(&sys_id).copied().unwrap_or(0)
            });
            Some(Ok(PyRep::Integer(lp)))
        }
        ("facWarMgr", "GetStats_CorpPilots") => {
            let corp_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.corporation_id)
                .unwrap_or(0);
            let (chars, _, _, _) = compute_fw_leaderboards(ctx);
            let rows = ctx.db.with_snapshot(|s| {
                let mut list = Vec::new();
                for (&cid, st) in &chars {
                    if s.characters.get(&cid).is_none_or(|c| c.corporation_id == corp_id || corp_id == 0) {
                        list.push(PyRep::keyval([
                            ("characterID", PyRep::Integer(cid)),
                            ("killsY", PyRep::Integer(st.vals[0])),
                            ("killsW", PyRep::Integer(st.vals[1])),
                            ("killsTotal", PyRep::Integer(st.vals[2])),
                            ("vpY", PyRep::Integer(st.vals[3])),
                            ("vpW", PyRep::Integer(st.vals[4])),
                            ("vpTotal", PyRep::Integer(st.vals[5])),
                        ]));
                    }
                }
                list
            });
            Some(Ok(PyRep::List(rows)))
        }
        ("facWarMgr", "GetStats_FactionInfo") => Some(handle_fw_stats_faction_info(ctx)),
        ("facWarMgr", "GetStats_Militia") => Some(handle_fw_stats_militia(ctx)),
        ("facWarMgr", "RefreshCorps") => Some(handle_fw_refresh_corps(ctx, session)),
        ("facWarMgr", "WithdrawJoinFactionAsAlliance") => {
            Some(handle_fw_withdraw_faction(ctx, session, true, false, args))
        }
        ("facWarMgr", "WithdrawJoinFactionAsCorporation") => {
            Some(handle_fw_withdraw_faction(ctx, session, false, false, args))
        }
        ("facWarMgr", "WithdrawLeaveFactionAsAlliance") => {
            Some(handle_fw_withdraw_faction(ctx, session, true, true, args))
        }
        ("facWarMgr", "WithdrawLeaveFactionAsCorporation") => {
            Some(handle_fw_withdraw_faction(ctx, session, false, true, args))
        }
        _ => None,
    }
}

const FW_PENDING_STORE_KEY: i64 = -10_000_004;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct FwPendingQueue {
    #[serde(default)]
    pub corp_joins: HashMap<i64, i64>,
    #[serde(default)]
    pub corp_leaves: HashMap<i64, i64>,
    #[serde(default)]
    pub alliance_joins: HashMap<i64, i64>,
    #[serde(default)]
    pub alliance_leaves: HashMap<i64, i64>,
}

fn load_fw_pending(snap: &eve_db::WorldStateSnapshot) -> FwPendingQueue {
    snap.item_custom_names
        .get(&FW_PENDING_STORE_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

fn save_fw_pending(snap: &mut eve_db::WorldStateSnapshot, st: &FwPendingQueue) {
    if let Ok(json) = serde_json::to_string(st) {
        snap.item_custom_names.insert(FW_PENDING_STORE_KEY, json);
    }
}

fn handle_fw_stats_top_and_all(ctx: &ServerContext) -> Result<PyRep, BridgeRpcError> {
    let (chars, corps, facs, allys) = compute_fw_leaderboards(ctx);
    let (char_sub, char_top, char_all) = build_entity_subtables("characterID", &chars);
    let (corp_sub, corp_top, corp_all) = build_entity_subtables("corporationID", &corps);
    let (fac_sub, fac_top, fac_all) = build_entity_subtables("factionID", &facs);
    let (ally_sub, ally_top, ally_all) = build_entity_subtables("allianceID", &allys);
    let top_groups = PyRep::Dict(vec![
        (PyRep::Integer(1), char_top.to_py_dict()),
        (PyRep::Integer(2), corp_top.to_py_dict()),
        (PyRep::Integer(19), fac_top.to_py_dict()),
        (PyRep::Integer(32), ally_top.to_py_dict()),
    ]);
    let all_groups = PyRep::Dict(vec![
        (PyRep::Integer(1), char_all.to_py_dict()),
        (PyRep::Integer(2), corp_all.to_py_dict()),
        (PyRep::Integer(19), fac_all.to_py_dict()),
        (PyRep::Integer(32), ally_all.to_py_dict()),
    ]);
    Ok(PyRep::Dict(vec![
        (PyRep::Integer(0), top_groups),
        (PyRep::Integer(1), all_groups),
        (PyRep::String("characters".into()), char_sub),
        (PyRep::String("corporations".into()), corp_sub),
        (PyRep::String("factions".into()), fac_sub),
        (PyRep::String("alliances".into()), ally_sub),
    ]))
}

fn handle_fw_stats_character(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let cid = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.character_id)
        .unwrap_or(0);
    let (chars, _, _, _) = compute_fw_leaderboards(ctx);
    Ok(chars.get(&cid).copied().unwrap_or_default().to_py_dict())
}

fn handle_fw_stats_corp(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let cid = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.corporation_id)
        .unwrap_or(0);
    let (_, corps, _, _) = compute_fw_leaderboards(ctx);
    Ok(corps.get(&cid).copied().unwrap_or_default().to_py_dict())
}

fn handle_fw_stats_alliance(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let aid = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.alliance_id)
        .unwrap_or(0);
    let (_, _, _, allys) = compute_fw_leaderboards(ctx);
    Ok(allys.get(&aid).copied().unwrap_or_default().to_py_dict())
}

fn handle_fw_stats_militia(ctx: &ServerContext) -> Result<PyRep, BridgeRpcError> {
    let (_, _, facs, _) = compute_fw_leaderboards(ctx);
    let mut entries = Vec::new();
    for fid in [500_001_i64, 500_002, 500_003, 500_004, 500_010, 500_011] {
        let st = facs.get(&fid).copied().unwrap_or_default();
        entries.push((PyRep::Integer(fid), st.to_py_dict()));
    }
    Ok(PyRep::Dict(entries))
}

fn handle_fw_stats_faction_info(ctx: &ServerContext) -> Result<PyRep, BridgeRpcError> {
    let dict = ctx.db.with_snapshot(|s| {
        let caldari_gal_total = s
            .fw_system_occupations
            .values()
            .filter(|o| matches!(o.occupier_faction_id, 500_001 | 500_004))
            .count();
        let min_amarr_total = s
            .fw_system_occupations
            .values()
            .filter(|o| matches!(o.occupier_faction_id, 500_002 | 500_003))
            .count();
        let pirate_total = s
            .fw_system_occupations
            .values()
            .filter(|o| matches!(o.occupier_faction_id, 500_010 | 500_011))
            .count();
        let mut out = Vec::new();
        for fid in [500_001_i64, 500_002, 500_003, 500_004, 500_010, 500_011] {
            let members = s
                .characters
                .values()
                .filter(|c| c.war_faction_id == Some(fid))
                .count() as i64;
            let systems = s
                .fw_system_occupations
                .values()
                .filter(|o| o.occupier_faction_id == fid)
                .count() as i64;
            let warzone_total = match fid {
                500_001 | 500_004 => caldari_gal_total,
                500_002 | 500_003 => min_amarr_total,
                _ => pirate_total,
            };
            let warzone_controlled = if warzone_total > 0 {
                (systems as f64) / (warzone_total as f64)
            } else {
                0.0
            };
            out.push((
                PyRep::Integer(fid),
                PyRep::keyval([
                    ("factionID", PyRep::Integer(fid)),
                    ("militiaMembersCount", PyRep::Integer(members)),
                    ("systemsCount", PyRep::Integer(systems)),
                    ("warzoneControlled", PyRep::Real(warzone_controlled)),
                ]),
            ));
        }
        PyRep::Dict(out)
    });
    Ok(dict)
}

fn handle_donate_lps_to_solar_system(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(sys_id) = args
        .first()
        .and_then(|a| a.as_i64())
        .and_then(|v| u32::try_from(v).ok())
        .or(session.solar_system_id)
    else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "No solar system specified".into(),
            client_error: None,
        });
    };
    let Some(amt) = args.get(1).and_then(|a| a.as_i64()).filter(|&a| a > 0) else {
        return Err(BridgeRpcError {
            code: "InvalidLpDonation".into(),
            message: "LP donation amount must be positive".into(),
            client_error: None,
        });
    };
    donate_lp_to_fw_ihub(ctx, char_id, sys_id, amt).map(|(tot, tier, tax, fac)| {
        PyRep::keyval([
            ("totalLP", PyRep::Integer(tot)),
            ("systemTier", PyRep::Integer(tier as i64)),
            ("tier", PyRep::Integer(tier as i64)),
            ("taxReduction", PyRep::Real(tax)),
            ("facilityBonus", PyRep::Real(fac)),
        ])
    })
}

fn handle_fw_refresh_corps(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    ctx.db.with_mut_snapshot(|snap| {
        let alliances: Vec<(i64, Vec<i64>)> = snap
            .alliances
            .values()
            .filter_map(|a| {
                snap.fw_runtime
                    .corp_war_factions
                    .get(&a.executor_corp_id)
                    .copied()
                    .map(|fid| (fid, a.member_corp_ids.iter().copied().collect::<Vec<i64>>()))
            })
            .collect();
        for (fid, member_corps) in alliances {
            for mcid in member_corps {
                snap.fw_runtime.corp_war_factions.entry(mcid).or_insert(fid);
            }
        }
    });
    Ok(PyRep::None)
}

fn handle_fw_withdraw_faction(
    ctx: &ServerContext,
    session: &SessionState,
    is_alliance: bool,
    is_leave: bool,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let requested_fac = args.first().and_then(|a| a.as_i64());
    ctx.db.try_with_mut_snapshot(|snap| {
        let cid = session
            .corporation_id
            .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id))
            .unwrap_or(0);
        let (corp_ceo, corp_roles) = snap.effective_corp_roles_at(
            cid,
            char_id,
            None,
            session.corp_role,
            ctx.now_unix(),
        );
        let is_corp_officer = corp_ceo
            || (corp_roles & eve_db::corp_roles::ROLE_DIRECTOR) != 0;
        if !is_corp_officer {
            return Err(BridgeRpcError {
                code: "AccessDenied".into(),
                message: "Only corporation CEO or Director can withdraw militia requests".into(),
                client_error: None,
            });
        }
        let mut pending = load_fw_pending(snap);
        let removed = if is_alliance {
            let aid = session
                .alliance_id
                .or_else(|| snap.characters.get(&char_id).and_then(|c| c.alliance_id))
                .ok_or_else(|| BridgeRpcError {
                    code: "NotInAlliance".into(),
                    message: "Corporation is not in an alliance".into(),
                    client_error: None,
                })?;
            let is_exec = snap
                .alliances
                .get(&aid)
                .is_some_and(|a| a.executor_corp_id == cid);
            if !is_exec {
                return Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Only the alliance executor corporation can withdraw alliance militia requests".into(),
                    client_error: None,
                });
            }
            let map = if is_leave {
                &mut pending.alliance_leaves
            } else {
                &mut pending.alliance_joins
            };
            match map.get(&aid).copied() {
                Some(pending_fac) if requested_fac.is_none_or(|rf| rf == pending_fac) => {
                    map.remove(&aid);
                    true
                }
                _ => false,
            }
        } else {
            let map = if is_leave {
                &mut pending.corp_leaves
            } else {
                &mut pending.corp_joins
            };
            match map.get(&cid).copied() {
                Some(pending_fac) if requested_fac.is_none_or(|rf| rf == pending_fac) => {
                    map.remove(&cid);
                    true
                }
                _ => false,
            }
        };
        if removed {
            save_fw_pending(snap, &pending);
        }
        Ok(PyRep::Bool(removed))
    })
}

#[cfg(test)] #[path = "fw_gameplay_rpc_tests.rs"]
mod fw_gameplay_rpc_tests;

#[cfg(test)]
#[path = "fw_gameplay_rpc_edges_tests.rs"]
mod fw_gameplay_rpc_edges_tests;

