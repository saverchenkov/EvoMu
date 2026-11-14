//! `combat_npc_ai` — Extended NPC AI, Factions, Clone Soldiers, EWAR, Remote Repair, Bounties & Multi-Wave Encounters (`C-4`).

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{Ball, MovementMode};
use std::collections::HashSet;
use tracing::info;

/// Derives NPC behavioral archetype dynamically from the NPC's `type_id` and `group_id` / `category_id`
/// in `ctx.sde.types` / `ctx.sde.groups` rather than accepting an arbitrary string parameter.
pub fn derive_npc_archetype(ctx: &ServerContext, npc_type_id: u32) -> &'static str {
    let sde_type = ctx.sde.types.get(&npc_type_id);
    let group = sde_type.and_then(|t| ctx.sde.groups.get(&t.group_id));

    let type_name = sde_type.map(|t| t.name.as_str()).unwrap_or("");
    let group_name = group.map(|g| g.name.as_str()).unwrap_or("");
    let group_id = sde_type.map(|t| t.group_id).unwrap_or(0);
    let faction_id = sde_type.and_then(|t| t.faction_id);
    let meta_group_id = sde_type.and_then(|t| t.meta_group_id);
    let meta_level = sde_type.and_then(|t| t.meta_level).unwrap_or(0);

    // 1. Pochven Hypnosian Sleepers & Decloaked Transmission Relay / Sleeper Sentry Towers
    if npc_type_id == 41_589 {
        return "TransmissionRelay";
    }
    if crate::services::wormhole_pve_escalations::is_sleeper_sentry_type(npc_type_id) {
        return "SleeperSentry";
    }
    if type_name.contains("Hypnosian") || npc_type_id == 56680 || (56319..=56327).contains(&npc_type_id) {
        return "PochvenHypnosianSleeper";
    }

    // 1b. Zarzakh gate guns and Deathless / Outer Zarzakh pirate patrol fleets
    if matches!(npc_type_id, 78_890 | 79_183 | 79_184 | 79_185 | 73_815)
        || (group_id == 383 && type_name.contains("Deathless Retribution"))
    {
        return "ZarzakhGateGun";
    }
    if group_id == 4636
        || matches!(
            npc_type_id,
            78_891..=78_898 | 80_806..=80_809 | 80_448..=80_452 | 73_818
        )
    {
        if type_name.contains("Outer Zarzakh") || (80_448..=80_452).contains(&npc_type_id) {
            return "OuterZarzakhPatrol";
        }
        return "ZarzakhPatrol";
    }

    // 2. Sleepers: Sleeper group IDs (e.g. 959..=961, 982..=1005), Sleeper factions (500021, 500022), or group/type name
    if group_name.contains("Sleeper")
        || type_name.contains("Sleeper")
        || (959..=961).contains(&group_id)
        || (982..=1005).contains(&group_id)
        || matches!(faction_id, Some(500021 | 500022))
    {
        return "Sleeper";
    }

    // 3. Rogue Drones: Rogue Drone faction (500008), groups (287, 755..=761, 801..=805, 1174), or group name
    if group_name.contains("Rogue Drone")
        || matches!(faction_id, Some(500008))
        || group_id == 287
        || (755..=761).contains(&group_id)
        || (801..=805).contains(&group_id)
        || group_id == 1174
        || type_name.contains("Alvi")
        || type_name.contains("Alvus")
    {
        return "RogueDrone";
    }

    // 3b. Empire Navy / Republic Fleet / Faction Police (must precede group_id == 4034 EDENCOM check,
    // because Empire Navy & Republic Fleet hulls 52923..=52967 / 54000..=54107 share SDE group 4034 and faction_id 500027)
    if group_name.contains("Faction Drone")
        || group_name.contains("Navy")
        || type_name.contains("Navy")
        || type_name.contains("Republic Fleet")
        || group_name.contains("Police")
        || type_name.contains("Police")
        || (52923..=52967).contains(&npc_type_id)
        || (54000..=54107).contains(&npc_type_id)
    {
        return "Navy";
    }

    // 4. EDENCOM: EDENCOM faction (500027), group (4034), or group/type name
    if group_name.contains("EDENCOM")
        || type_name.contains("EDENCOM")
        || matches!(faction_id, Some(500027))
        || group_id == 4034
    {
        return "EDENCOM";
    }

    // 5. Incursion Sansha: Incursion Sansha groups (1051..=1056), or group/type name
    if group_name.contains("Incursion")
        || (1051..=1056).contains(&group_id)
        || type_name.starts_with("Outuni")
        || type_name.starts_with("Kupusam")
        || type_name.starts_with("Deltole")
    {
        return "IncursionSansha";
    }

    // 5b. Mordu's Legion Special Warfare Units (groups 1285..=1287)
    if (1285..=1287).contains(&group_id) || (33864..=33866).contains(&npc_type_id) {
        return "MorduCommander";
    }

    // 5c. Pirate Insurgency Roaming Pirates (group 4637) & Roaming Enforcers (group 4638)
    // Must precede Officer/Commander checks so high-tier/Commander-named group 4637/4638 hulls stay classified as roamers.
    if group_id == 4637 {
        return "InsurgencyPirateRoamer";
    }
    if group_id == 4638 {
        return "InsurgencyEnforcer";
    }

    // 6. Officer / Commander: SDE group name containing Officer/Commander, meta_group_id, or meta_level >= 11
    if group_name.contains("Officer")
        || group_name.contains("Commander")
        || (4795..=4804).contains(&group_id)
        || meta_group_id == Some(4) // Faction
        || meta_group_id == Some(5) // Officer
        || meta_level >= 11
    {
        return "Officer";
    }

    // 7. Drifters: Drifter faction (500024), groups (1310, 1530, 1788, 1956, 4035), or group/type name
    if group_name.contains("Drifter")
        || type_name.contains("Drifter")
        || matches!(faction_id, Some(500024))
        || group_id == 1310
        || group_id == 1530
        || group_id == 1788
        || group_id == 1956
        || group_id == 4035
    {
        return "Drifter";
    }

    // 8. Triglavians: Triglavian faction (500026), group (4028), or group/type name
    if group_name.contains("Triglavian")
        || type_name.contains("Triglavian")
        || matches!(faction_id, Some(500026))
        || group_id == 4028
        || type_name.contains("Damavik")
        || type_name.contains("Vedmak")
        || type_name.contains("Drekavac")
        || type_name.contains("Leshak")
        || type_name.contains("Raznaborg")
    {
        return "Triglavian";
    }

    // 9. Clone Soldier: type name or group name
    if type_name.contains("Clone Soldier") || group_name.contains("Clone Soldier") {
        return "CloneSoldier";
    }

    // 10. Hauler / Mining Hauler
    if group_name.contains("Hauler")
        || group_name.contains("Industrial")
        || type_name.contains("Hauler")
        || type_name.starts_with('♦')
        || group_id == 1767
    {
        return "Hauler";
    }

    // 11. Navy / Faction Police / Faction
    if group_name.contains("Faction Drone")
        || group_name.contains("Navy")
        || type_name.contains("Navy")
        || group_name.contains("Police")
        || type_name.contains("Police")
        || matches!(faction_id, Some(500001 | 500002 | 500003 | 500004))
    {
        return "Navy";
    }

    "Pirate"
}

/// Deterministically resolves the lowest-ID SDE NPC corporation belonging to `faction_id`,
/// returning `-1` when no matching corporation exists so Destiny wire encoding emits `None`
/// for `corpID` rather than leaking a `500_xxx` faction ID into `cfg.crptickernames.Get`.
pub(crate) fn resolve_npc_corp_id_for_sde_faction(
    sde: &eve_sde::SdeRegistry,
    faction_id: i64,
) -> i64 {
    let Ok(fid) = u32::try_from(faction_id) else {
        return -1;
    };
    sde.npc_corporations
        .values()
        .filter(|c| c.faction_id == Some(fid))
        .map(|c| c.corporation_id)
        .min()
        .unwrap_or(-1)
}

/// ServerContext convenience wrapper for [`resolve_npc_corp_id_for_sde_faction`].
pub(crate) fn resolve_npc_corp_id_for_faction(ctx: &ServerContext, faction_id: i64) -> i64 {
    resolve_npc_corp_id_for_sde_faction(&ctx.sde, faction_id)
}

/// Spawns an NPC combat entity from SDE, dynamically determining its archetype from SDE metadata.
pub async fn spawn_npc_entity_from_sde(
    ctx: &ServerContext,
    solar_system_id: u32,
    npc_type_id: u32,
    spawn_group_id: u32,
    pos: DVec3,
) -> i64 {
    spawn_specialized_npc_entity(ctx, solar_system_id, npc_type_id, "", spawn_group_id, pos).await
}

/// Spawns multiple specialized NPC combat entities using a caller-supplied starting ball ID,
/// appending `(ball_id, entity)` to `out_entities` without committing to `db` or `combat_ephemeral`.
pub(crate) async fn spawn_specialized_npc_batch_with_start_id<F>(
    ctx: &ServerContext,
    solar_system_id: u32,
    specs: &[(u32, &str, u32, DVec3)],
    start_ball_id: i64,
    mut post_build: F,
    out_entities: &mut Vec<(i64, ExtendedNpcEntity)>,
) -> Vec<i64>
where
    F: FnMut(&mut ExtendedNpcEntity),
{
    if specs.is_empty() {
        return Vec::new();
    }
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let mut spawned_ids = Vec::with_capacity(specs.len());
    out_entities.reserve(specs.len());
    for (idx, &(npc_type_id, archetype_hint, spawn_group_id, pos)) in specs.iter().enumerate() {
        let ball_id = start_ball_id + idx as i64;
        let sde_derived = derive_npc_archetype(ctx, npc_type_id);
        let archetype = if !archetype_hint.is_empty()
            && (sde_derived == "Pirate"
                || matches!(
                    archetype_hint,
                    "FactionPolice" | "Police" | "Sleeper" | "CounterInsurgencySentry" | "Navy"
                    | "CaldariNavy" | "MinmatarNavy" | "AmarrNavy" | "GallenteNavy"
                    | "ZarzakhGateGun" | "ZarzakhPatrol" | "OuterZarzakhPatrol"
                ))
        {
            archetype_hint
        } else {
            sde_derived
        };
        let (ball, cs, mut entity) =
            crate::services::wormhole_pve_seeding::build_specialized_npc_components(
                ctx,
                solar_system_id,
                ball_id,
                npc_type_id,
                archetype,
                spawn_group_id,
                pos,
            );
        let sde_t = ctx.sde.types.get(&npc_type_id);
        let fac_owner = if entity.faction_id > 0 {
            entity.faction_id as i64
        } else {
            500_001
        };
        let npc_meta = eve_destiny::SlimItemMeta {
            group_id: sde_t.map(|t| t.group_id).unwrap_or(546),
            category_id: sde_t.map(|t| t.category_id).unwrap_or(11),
            owner_id: fac_owner,
            char_id: None,
            corp_id: resolve_npc_corp_id_for_faction(ctx, fac_owner),
            alliance_id: -1,
            security_status: -1.0,
            is_cloaked: false,
            name: sde_t.map(|t| t.name.clone()).filter(|n| !n.is_empty()),
            ..Default::default()
        };
        let _ = sys_handle.spawn_ball_with_meta(ball, cs, npc_meta).await;
        post_build(&mut entity);
        out_entities.push((ball_id, entity));
        spawned_ids.push(ball_id);
    }
    spawned_ids
}

/// Spawns multiple specialized NPC combat entities in one `next_entity_id` allocation and one
/// batched `with_mut_combat_ephemeral` persistence commit.
pub async fn spawn_specialized_npc_batch<F>(
    ctx: &ServerContext,
    solar_system_id: u32,
    specs: &[(u32, &str, u32, DVec3)],
    post_build: F,
) -> Vec<i64>
where
    F: FnMut(&mut ExtendedNpcEntity),
{
    if specs.is_empty() {
        return Vec::new();
    }
    let count = specs.len() as i64;
    let start_ball_id = ctx.db.with_mut_snapshot(|snap| {
        let start = snap.next_entity_id + 1;
        snap.next_entity_id += count;
        start
    });
    let mut built = Vec::with_capacity(specs.len());
    let spawned_ids = spawn_specialized_npc_batch_with_start_id(
        ctx,
        solar_system_id,
        specs,
        start_ball_id,
        post_build,
        &mut built,
    )
    .await;
    with_mut_combat_ephemeral(ctx, |rt| {
        for (ball_id, entity) in built {
            rt.npc_entities.insert(ball_id, entity);
        }
    });
    spawned_ids
}

/// Spawns a specialized NPC combat entity from SDE (`feat:npc.drifters`, `feat:npc.triglavians`, `feat:npc.sleepers`,
/// `feat:npc.edencom-npc`, `feat:npc.rogue-drones`, `feat:npc.incursion-npc`, `feat:npc.navy-npc`,
/// `feat:npc.clone-soldier-spawns`, `feat:npc.officer-npc`, `feat:npc.npc-hauler`).
pub async fn spawn_specialized_npc_entity(
    ctx: &ServerContext,
    solar_system_id: u32,
    npc_type_id: u32,
    archetype_hint: &str,
    spawn_group_id: u32,
    pos: DVec3,
) -> i64 {
    spawn_specialized_npc_batch(
        ctx,
        solar_system_id,
        &[(npc_type_id, archetype_hint, spawn_group_id, pos)],
        |_| {},
    )
    .await
    .into_iter()
    .next()
    .unwrap_or(0)
}

/// Determines if a candidate ball is hostile to EDENCOM or matches primary target hostility/fleet.
pub fn is_candidate_hostile_to_edencom(
    ctx: &ServerContext,
    npc: &ExtendedNpcEntity,
    cand_id: i64,
    target_id: i64,
) -> bool {
    let cand_char_opt = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .find(|c| c.active_ship_id == cand_id)
            .map(|c| (c.character_id, c.fleet_id))
    });
    if let Some((cid, cand_fleet_id)) = cand_char_opt {
        let standing = ctx.db.with_snapshot(|snap| {
            snap.characters
                .get(&cid)
                .and_then(|c| c.standings.get(&(npc.faction_id as i64)).copied())
                .unwrap_or(0.0)
        });
        if standing < 0.0 {
            return true;
        }
        let target_fleet_id = ctx.db.with_snapshot(|snap| {
            snap.characters
                .values()
                .find(|c| c.active_ship_id == target_id)
                .and_then(|c| c.fleet_id)
        });
        if let (Some(cfid), Some(tfid)) = (cand_fleet_id, target_fleet_id) {
            if cfid == tfid && cfid != 0 {
                return true;
            }
        }
        return false;
    }

    let drone_owner_opt = with_combat_ephemeral(ctx, |rt| {
        rt.active_drones_in_space.get(&cand_id).map(|d| d.owner_char_id)
    })
    .or_else(|| {
        ctx.db.with_snapshot(|snap| {
            snap.items
                .get(&cand_id)
                .and_then(|item| snap.characters.get(&item.owner_id))
                .map(|c| c.character_id)
        })
    });
    if let Some(owner_cid) = drone_owner_opt {
        let is_target_owner = ctx.db.with_snapshot(|snap| {
            snap.characters
                .get(&owner_cid)
                .map(|c| c.active_ship_id == target_id)
                .unwrap_or(false)
        });
        if is_target_owner {
            return true;
        }
        let standing = ctx.db.with_snapshot(|snap| {
            snap.characters
                .get(&owner_cid)
                .and_then(|c| c.standings.get(&(npc.faction_id as i64)).copied())
                .unwrap_or(0.0)
        });
        return standing < 0.0;
    }

    let other_npc_opt = with_combat_ephemeral(ctx, |rt| rt.npc_entities.get(&cand_id).cloned());
    if let Some(other_npc) = other_npc_opt {
        if matches!(
            other_npc.archetype.as_str(),
            "Triglavian" | "Sleeper" | "PochvenHypnosianSleeper" | "Drifter" | "RogueDrone" | "BeltRat" | "Officer" | "Pirate"
        ) {
            return true;
        }
        if matches!(other_npc.faction_id, 500026 | 500024 | 500021 | 500025 | 500008 | 500010..=500020) {
            return true;
        }
        return false;
    }

    false
}

/// Steps extended NPC AI (`feat:npc.npc-ai-targeting`, `feat:npc.npc-ewar-application`, `feat:npc.npc-remote-repair`,
/// `feat:npc.npc-target-switching`, `feat:npc.npc-standings-aggression`, `feat:npc.drifters`, `feat:npc.triglavians`,
/// `feat:npc.sleepers`, `feat:npc.edencom-npc`).
#[derive(Clone, Copy)]
struct BallSnapshotLite {
    position: DVec3,
    type_id: u32,
    speed_fraction: f64,
    radius: f64,
    max_velocity: f64,
    mode_is_warp: bool,
    is_cloaked_or_dead: bool,
}

struct CandidateOwnerMeta {
    standings: HashMap<i64, f64>,
    is_low_sec_criminal: bool,
    war_faction_id: Option<i64>,
    is_capsule: bool,
    owner_active_ship_id: Option<i64>,
    has_criminal_flag: bool,
    has_suspect_flag: bool,
    has_disapproval_flag: bool,
    pilot_is_pirate: bool,
    pilot_is_campaign_pirate: bool,
    is_active_player_ship: bool,
}

struct PrecomputedCandidate {
    cand_id: i64,
    position: DVec3,
    is_player_drone: bool,
    is_capital: bool,
    sig_radius: f64,
    has_ewar: bool,
    has_logi: bool,
    drone_sig: f64,
    drone_state: Option<(i64, Option<i64>, f64)>,
    owner: Option<CandidateOwnerMeta>,
}

pub fn step_extended_npc_ai_sync(ctx: &ServerContext, solar_system_id: u32) {
    crate::services::mobile_deployables_ext::step_system_deployables_sync(ctx, solar_system_id);
    let (mut npcs_in_sys, has_scram, prev_npc_scram_targets, is_debugged) = {
        let guard = ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
        let npcs: Vec<_> = guard
            .npc_entities
            .values()
            .filter(|n| n.solar_system_id == solar_system_id)
            .cloned()
            .collect();
        let scram_opt = guard.npc_scrambled_ships.get(&solar_system_id);
        let has_scram = scram_opt.is_some_and(|s| !s.is_empty());
        if npcs.is_empty() && !has_scram {
            return;
        }
        let prev_scram = scram_opt.cloned().unwrap_or_default();
        let debugged = !guard.behavior_debugging_active.is_empty();
        (npcs, has_scram, prev_scram, debugged)
    };
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);

    let (lite_balls, cand_ball_ids, mut dead_npc_set): (
        HashMap<i64, BallSnapshotLite>,
        Vec<i64>,
        HashSet<i64>,
    ) = {
        let bp = sys_handle.ballpark.read().unwrap_or_else(|e| e.into_inner());
        let mut lites = HashMap::new();
        let mut cand_ids = Vec::new();
        let mut dead_npcs = HashSet::new();

        let to_lite = |id: i64, b: &Ball| -> BallSnapshotLite {
            let is_cloaked_or_dead = bp
                .combat_states
                .get(&id)
                .is_some_and(|cs| cs.is_cloaked || cs.structure_hp <= 0.0);
            BallSnapshotLite {
                position: b.position,
                type_id: b.type_id,
                speed_fraction: b.speed_fraction,
                radius: b.radius,
                max_velocity: b.max_velocity,
                mode_is_warp: matches!(b.mode, MovementMode::Warp { .. }),
                is_cloaked_or_dead,
            }
        };

        for npc in &npcs_in_sys {
            let npc_dead = bp
                .combat_states
                .get(&npc.ball_id)
                .is_some_and(|cs| cs.structure_hp <= 0.0);
            if npc_dead {
                dead_npcs.insert(npc.ball_id);
            } else if let Some(b) = bp.balls.get(&npc.ball_id) {
                lites.entry(npc.ball_id).or_insert_with(|| to_lite(npc.ball_id, b));
            } else {
                dead_npcs.insert(npc.ball_id);
            }
            for &tid in npc
                .aggression_threat
                .keys()
                .chain(npc.threat_table.keys())
                .chain(npc.target_id.iter())
            {
                if !lites.contains_key(&tid) {
                    if let Some(tb) = bp.balls.get(&tid) {
                        lites.insert(tid, to_lite(tid, tb));
                    }
                }
            }
        }

        if dead_npcs.len() < npcs_in_sys.len() {
            for (&id, b) in &bp.balls {
                let is_targetable_cat = ctx
                    .sde
                    .types
                    .get(&b.type_id)
                    .is_none_or(|t| matches!(t.category_id, 6 | 11 | 18 | 65));
                if !is_targetable_cat {
                    continue;
                }
                let lite = *lites.entry(id).or_insert_with(|| to_lite(id, b));
                if b.type_id != 26468 && !lite.mode_is_warp && !lite.is_cloaked_or_dead {
                    cand_ids.push(id);
                }
            }
        }

        (lites, cand_ids, dead_npcs)
    };

    let mut non_npc_drones: Vec<(i64, Option<(u32, i64, Option<i64>, f64)>)> = Vec::new();
    if !cand_ball_ids.is_empty() {
        with_combat_ephemeral(ctx, |rt| {
            for cid in cand_ball_ids {
                if !rt.npc_entities.contains_key(&cid) {
                    let drone_info = rt
                        .active_drones_in_space
                        .get(&cid)
                        .map(|d| (d.type_id, d.activity_state, d.target_id, d.damage_vector.total()));
                    non_npc_drones.push((cid, drone_info));
                }
            }
        });
    }

    let now_unix = ctx.now_unix();
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|sys| sys.security_status)
        .unwrap_or(0.0);
    let sec_threshold = faction_police_sec_threshold(sys_sec).unwrap_or(-5.0);

    let precomputed_cands: Vec<PrecomputedCandidate> = if non_npc_drones.is_empty() {
        Vec::new()
    } else {
        struct RawCand {
            cand_id: i64,
            position: DVec3,
            is_player_drone: bool,
            is_capital: bool,
            sig_radius: f64,
            has_ewar: bool,
            has_logi: bool,
            drone_sig: f64,
            drone_state: Option<(i64, Option<i64>, f64)>,
            owner_cid: Option<i64>,
            standings: HashMap<i64, f64>,
            is_low_sec_criminal: bool,
            war_faction_id: Option<i64>,
            is_capsule: bool,
            owner_active_ship_id: Option<i64>,
            is_active_player_ship: bool,
        }

        let mut raws: Vec<RawCand> = ctx.db.with_snapshot(|snap| {
            let mut ewar_locs: HashSet<i64> = HashSet::new();
            let mut logi_locs: HashSet<i64> = HashSet::new();
            let cand_id_set: HashSet<i64> = non_npc_drones.iter().map(|&(id, _)| id).collect();
            if !snap.module_active_states.is_empty() {
                for &aid in &snap.module_active_states {
                    if let Some(it) = snap.items.get(&aid) {
                        if cand_id_set.contains(&it.location_id) {
                            if let Some(mt) = ctx.sde.types.get(&it.type_id) {
                                if matches!(mt.group_id, 52 | 65 | 201 | 208 | 291 | 357) {
                                    ewar_locs.insert(it.location_id);
                                }
                                if matches!(mt.group_id, 41 | 325 | 585) {
                                    logi_locs.insert(it.location_id);
                                }
                            }
                        }
                    }
                }
            }

            let mut active_ship_to_char: HashMap<i64, i64> = HashMap::new();
            for ch in snap.characters.values() {
                if cand_id_set.contains(&ch.active_ship_id) {
                    active_ship_to_char
                        .entry(ch.active_ship_id)
                        .or_insert(ch.character_id);
                }
            }

            let mut out = Vec::new();
            for (cand_id, drone_info) in non_npc_drones {
                let Some(lite) = lite_balls.get(&cand_id) else {
                    continue;
                };
                let is_player_drone = drone_info.is_some()
                    || snap.items.get(&cand_id).is_some_and(|item| {
                        ctx.sde
                            .types
                            .get(&item.type_id)
                            .is_some_and(|t| t.category_id == 18 || matches!(t.group_id, 100 | 549 | 1023))
                    });
                if lite.max_velocity <= 0.0 && !is_player_drone {
                    continue;
                }
                let (owner_cid, is_active_player_ship) = if is_player_drone {
                    let ocid = snap
                        .items
                        .get(&cand_id)
                        .and_then(|item| snap.characters.get(&item.owner_id))
                        .map(|c| c.character_id);
                    (ocid, false)
                } else {
                    let ocid = active_ship_to_char.get(&cand_id).copied();
                    (ocid, ocid.is_some())
                };

                let item_opt = snap.items.get(&cand_id);
                let st = item_opt.and_then(|i| ctx.sde.types.get(&i.type_id));
                let is_capital = st.is_some_and(|t| matches!(t.group_id, 485 | 547 | 1538 | 30 | 659));
                let sig_radius = st
                    .and_then(|t| t.default_attributes.get(&552).copied())
                    .unwrap_or(40.0);
                let has_ewar = ewar_locs.contains(&cand_id);
                let has_logi = logi_locs.contains(&cand_id);
                let is_capsule = item_opt
                    .map(|i| {
                        i.type_id == 670
                            || ctx.sde.types.get(&i.type_id).map(|t| t.group_id == 29).unwrap_or(false)
                    })
                    .unwrap_or(false);

                let (drone_sig, drone_state) = if is_player_drone {
                    let d_tid = drone_info.map(|d| d.0).unwrap_or(lite.type_id);
                    let d_sig = ctx
                        .sde
                        .types
                        .get(&d_tid)
                        .and_then(|t| t.default_attributes.get(&552).copied())
                        .unwrap_or(25.0);
                    let d_state = drone_info.map(|(_, act, tid, dps)| (act, tid, dps));
                    (d_sig, d_state)
                } else {
                    (0.0, None)
                };

                let (standings, is_low_sec_criminal, war_faction_id, owner_active_ship_id) =
                    if let Some(cid) = owner_cid {
                        let char_rec = snap.characters.get(&cid);
                        let s = char_rec.map(|c| c.standings.clone()).unwrap_or_default();
                        let sec = char_rec.map(|c| c.security_status).unwrap_or(0.0);
                        let wf = char_rec.and_then(|c| c.war_faction_id);
                        let osid = char_rec.map(|c| c.active_ship_id);
                        (s, sec <= sec_threshold || sec <= -5.0, wf, osid)
                    } else {
                        (HashMap::new(), false, None, None)
                    };

                out.push(RawCand {
                    cand_id,
                    position: lite.position,
                    is_player_drone,
                    is_capital,
                    sig_radius,
                    has_ewar,
                    has_logi,
                    drone_sig,
                    drone_state,
                    owner_cid,
                    standings,
                    is_low_sec_criminal,
                    war_faction_id,
                    is_capsule,
                    owner_active_ship_id,
                    is_active_player_ship,
                });
            }
            out
        });

        if raws.iter().any(|r| r.is_player_drone && r.owner_cid.is_none()) {
            let fallback_owners: HashMap<i64, i64> = with_combat_ephemeral(ctx, |rt| {
                raws.iter()
                    .filter(|r| r.is_player_drone && r.owner_cid.is_none())
                    .filter_map(|r| {
                        rt.active_drones_in_space
                            .get(&r.cand_id)
                            .map(|d| (r.cand_id, d.owner_char_id))
                    })
                    .collect()
            });
            if !fallback_owners.is_empty() {
                ctx.db.with_snapshot(|snap| {
                    for r in &mut raws {
                        if r.owner_cid.is_none() {
                            if let Some(&cid) = fallback_owners.get(&r.cand_id) {
                                r.owner_cid = Some(cid);
                                let char_rec = snap.characters.get(&cid);
                                r.standings = char_rec.map(|c| c.standings.clone()).unwrap_or_default();
                                let sec = char_rec.map(|c| c.security_status).unwrap_or(0.0);
                                r.is_low_sec_criminal = sec <= sec_threshold || sec <= -5.0;
                                r.war_faction_id = char_rec.and_then(|c| c.war_faction_id);
                                r.owner_active_ship_id = char_rec.map(|c| c.active_ship_id);
                            }
                        }
                    }
                });
            }
        }

        let owner_cids: Vec<i64> = {
            let mut s: HashSet<i64> = HashSet::new();
            for r in &raws {
                if let Some(cid) = r.owner_cid {
                    s.insert(cid);
                }
            }
            s.into_iter().collect()
        };

        let cw_flags: HashMap<i64, (bool, bool)> = if owner_cids.is_empty() {
            HashMap::new()
        } else if let Ok(cw) = ctx.crimewatch.read() {
            owner_cids
                .iter()
                .map(|&cid| {
                    let st = cw.states.get(&cid);
                    (
                        cid,
                        (
                            st.is_some_and(|s| s.is_criminal(now_unix)),
                            st.is_some_and(|s| s.is_suspect(now_unix)),
                        ),
                    )
                })
                .collect()
        } else {
            HashMap::new()
        };

        let disapproval_cids: HashSet<i64> = if owner_cids.is_empty() {
            HashSet::new()
        } else {
            crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                owner_cids
                    .iter()
                    .copied()
                    .filter(|cid| rt.disapproval_flags.get(cid).copied().is_some_and(|exp| now_unix < exp))
                    .collect()
            })
        };

        let campaign_pirate_cids: HashMap<i64, bool> = owner_cids
            .iter()
            .map(|&cid| {
                (
                    cid,
                    crate::services::security_mgr::is_pilot_aligned_with_insurgency_pirate(
                        ctx,
                        solar_system_id,
                        cid,
                    ),
                )
            })
            .collect();

        raws.into_iter()
            .map(|r| {
                let owner = r.owner_cid.map(|cid| {
                    let (has_criminal_flag, has_suspect_flag) =
                        cw_flags.get(&cid).copied().unwrap_or((false, false));
                    CandidateOwnerMeta {
                        standings: r.standings,
                        is_low_sec_criminal: r.is_low_sec_criminal,
                        war_faction_id: r.war_faction_id,
                        is_capsule: r.is_capsule,
                        owner_active_ship_id: r.owner_active_ship_id,
                        has_criminal_flag,
                        has_suspect_flag,
                        has_disapproval_flag: disapproval_cids.contains(&cid),
                        pilot_is_pirate: r
                            .war_faction_id
                            .is_some_and(crate::services::security_mgr::is_pirate_militia_faction),
                        pilot_is_campaign_pirate: campaign_pirate_cids
                            .get(&cid)
                            .copied()
                            .unwrap_or(false),
                        is_active_player_ship: r.is_active_player_ship,
                    }
                });
                PrecomputedCandidate {
                    cand_id: r.cand_id,
                    position: r.position,
                    is_player_drone: r.is_player_drone,
                    is_capital: r.is_capital,
                    sig_radius: r.sig_radius,
                    has_ewar: r.has_ewar,
                    has_logi: r.has_logi,
                    drone_sig: r.drone_sig,
                    drone_state: r.drone_state,
                    owner,
                }
            })
            .collect()
    };

    let cand_by_id: HashMap<i64, &PrecomputedCandidate> = precomputed_cands
        .iter()
        .map(|c| (c.cand_id, c))
        .collect();

    let mut active_npc_scram_targets: HashSet<i64> = HashSet::new();
    let mut dead_npcs_to_remove: Vec<i64> = Vec::new();
    let mut updated_npcs = Vec::new();
    let mut destroyed_in_call: HashSet<i64> = HashSet::new();
    let mut cached_c_stage = None;
    let mut cached_s_stage = None;

    let mut group_peer_ids_by_group: HashMap<u32, Vec<i64>> = HashMap::new();
    for n in &npcs_in_sys {
        if !dead_npc_set.contains(&n.ball_id) {
            group_peer_ids_by_group
                .entry(n.spawn_group_id)
                .or_default()
                .push(n.ball_id);
        }
    }

    for npc_val in npcs_in_sys.drain(..) {
        let nid = npc_val.ball_id;
        if dead_npc_set.contains(&nid) || destroyed_in_call.contains(&nid) {
            dead_npcs_to_remove.push(nid);
            continue;
        }
        let Some(&npc_ball) = lite_balls.get(&nid) else {
            dead_npc_set.insert(nid);
            dead_npcs_to_remove.push(nid);
            continue;
        };
        let orig_npc = npc_val.clone();
        let mut npc = npc_val;

        if is_debugged {
            info!(
                target: "ai::behavior",
                npc_id = nid,
                solar_system_id,
                archetype = %npc.archetype,
                target_id = ?npc.target_id,
                "NPC behavior decision execution tick"
            );
        }

        if npc.archetype == "TransmissionRelay"
            || npc.type_id == 41_589
            || npc_ball.mode_is_warp
        {
            if npc_ball.mode_is_warp && npc.target_id.is_some() {
                npc.target_id = None;
                updated_npcs.push((nid, npc));
            }
            continue;
        }

        // 0. Sleeper spawn leash (`feat:npc.sleeper-capacitor-immunity-and-leash`):
        // Pulling a Sleeper > 200 km from its spawn position triggers immediate teleport back to spawn.
        if (npc.archetype == "Sleeper" || npc.archetype == "PochvenHypnosianSleeper")
            && npc_ball.position.distance(npc.spawn_position) > 200_000.0
        {
            sys_handle.try_set_ball_position(nid, npc.spawn_position);
            continue;
        }

        // 1. Remote Repair (`feat:npc.npc-remote-repair`, `feat:npc.sleepers`):
        // If not ECM-jammed, repair damaged fellow NPCs in the same `spawn_group_id` within `remote_rep_range`.
        if !npc.ecm_jammed && (npc.remote_shield_rep > 0.0 || npc.remote_armor_rep > 0.0) {
            if let Some(peers) = group_peer_ids_by_group.get(&npc.spawn_group_id) {
                let peers_in_range: Vec<i64> = peers
                    .iter()
                    .copied()
                    .filter(|&pid| {
                        pid != nid
                            && !destroyed_in_call.contains(&pid)
                            && lite_balls
                                .get(&pid)
                                .is_some_and(|pb| npc_ball.position.distance(pb.position) <= npc.remote_rep_range)
                    })
                    .collect();
                if !peers_in_range.is_empty() {
                    let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
                    for pid in peers_in_range {
                        if let Some(pcs) = bp.combat_states.get_mut(&pid) {
                            if npc.remote_shield_rep > 0.0 && pcs.shield_hp < pcs.max_shield_hp {
                                pcs.shield_hp =
                                    (pcs.shield_hp + npc.remote_shield_rep).min(pcs.max_shield_hp);
                            }
                            if npc.remote_armor_rep > 0.0 && pcs.armor_hp < pcs.max_armor_hp {
                                pcs.armor_hp =
                                    (pcs.armor_hp + npc.remote_armor_rep).min(pcs.max_armor_hp);
                            }
                        }
                    }
                }
            }
        }

        // 2. Target Selection & Switching (`feat:npc.npc-target-switching`, `feat:npc.npc-standings-aggression`):
        for (&tid, &threat) in &npc.threat_table {
            npc.aggression_threat.entry(tid).or_insert(threat);
        }
        // Prune any targets in `aggression_threat` / `threat_table` that are no longer on grid (> 150 km, or > 250 km for sentries),
        // warped out (`MovementMode::Warp`), lost lock (`is_cloaked`), or destroyed (`structure_hp <= 0.0`).
        let max_engage_range = if npc.type_id == 79_507 {
            npc.optimal_range.max(500_000.0)
        } else if npc.archetype == "SleeperSentry" || npc.archetype == "ZarzakhGateGun" {
            npc.optimal_range.max(250_000.0)
        } else {
            150_000.0
        };
        let valid_target = |tid: i64| -> bool {
            if destroyed_in_call.contains(&tid) {
                return false;
            }
            let Some(tb) = lite_balls.get(&tid) else {
                return false;
            };
            tb.position.distance(npc_ball.position) <= max_engage_range
                && !tb.mode_is_warp
                && !tb.is_cloaked_or_dead
        };
        if !npc.aggression_threat.is_empty() {
            npc.aggression_threat.retain(|&tid, _| valid_target(tid));
        }
        if !npc.threat_table.is_empty() {
            npc.threat_table.retain(|&tid, _| valid_target(tid));
        }
        if npc.target_id.is_some_and(|tid| !valid_target(tid)) {
            npc.target_id = None;
        }

        // Add any hostile ships or active player drones within grid aggro range that pass standings aggression check
        if !precomputed_cands.is_empty() {
            let is_police = matches!(npc.archetype.as_str(), "FactionPolice" | "Police");
            let is_insurgency_pirate = npc.archetype == "InsurgencyPirateRoamer";
            let is_insurgency_enforcer = npc.archetype == "InsurgencyEnforcer";
            let is_ci_sentry = npc.archetype == "CounterInsurgencySentry" || npc.type_id == 79_507;
            let is_ambush_tackler = npc.archetype == "InsurgencyAmbushTackler";
            let is_ore_defender = npc.archetype == "InsurgencyAmbushOreDefender";
            let is_ore_rorqual = npc.type_id == 79_589 || npc.archetype == "InsurgencyOreRorqual";
            let is_outpost_defender = npc.archetype == "InsurgencyOutpostDefender";
            let is_navy = matches!(
                npc.archetype.as_str(),
                "Navy" | "CaldariNavy" | "MinmatarNavy" | "AmarrNavy" | "GallenteNavy"
            ) || (matches!(npc.faction_id, 500001..=500004)
                && !is_ci_sentry
                && !is_insurgency_enforcer
                && !matches!(npc.archetype.as_str(), "BeltRat" | "Pirate" | "FactionPolice" | "Police"));
            let is_pirate = matches!(
                npc.archetype.as_str(),
                "BeltRat" | "Pirate" | "Officer" | "CloneSoldier" | "MorduCommander"
            );
            let is_edencom = matches!(npc.archetype.as_str(), "EDENCOM" | "Edencom");
            let is_zarzakh_gun = npc.archetype == "ZarzakhGateGun";
            let is_zarzakh_patrol = matches!(npc.archetype.as_str(), "ZarzakhPatrol" | "OuterZarzakhPatrol");

            let max_acquire_range = if npc.type_id == 79_507 {
                npc.optimal_range.max(500_000.0)
            } else if npc.archetype == "SleeperSentry" || is_zarzakh_gun {
                npc.optimal_range.max(250_000.0)
            } else if npc.archetype == "Drifter" {
                npc.optimal_range.max(100_000.0)
            } else {
                75_000.0
            };
            let max_range = max_acquire_range.min(max_engage_range);

            for cand in &precomputed_cands {
                let cand_id = cand.cand_id;
                if cand_id == nid || destroyed_in_call.contains(&cand_id) {
                    continue;
                }
                let dist = npc_ball.position.distance(cand.position);
                if dist > max_range {
                    continue;
                }

                if let Some(owner) = cand.owner.as_ref() {
                    // Capsules (pods) are never engaged by Faction Police, Empire Navy, Insurgency Enforcers, or Counter-Insurgency Sentries
                    if (is_police || is_navy || is_insurgency_enforcer || is_ci_sentry) && owner.is_capsule {
                        continue;
                    }
                    let standing = owner
                        .standings
                        .get(&(npc.faction_id as i64))
                        .copied()
                        .unwrap_or(0.0);
                    let is_criminal = owner.is_low_sec_criminal || owner.has_criminal_flag;
                    let pilot_is_same_pirate = if npc.faction_id != 0 {
                        owner.war_faction_id == Some(i64::from(npc.faction_id))
                    } else {
                        owner.pilot_is_campaign_pirate
                    };
                    let owner_already_threat = owner.owner_active_ship_id.is_some_and(|osid| {
                        npc.aggression_threat.contains_key(&osid) || npc.threat_table.contains_key(&osid)
                    });

                    let should_skip = if is_pirate {
                        false
                    } else if is_insurgency_pirate {
                        let c_stage = *cached_c_stage.get_or_insert_with(|| {
                            crate::services::insurgency_services::insurgency_system_corruption_stage(
                                ctx,
                                solar_system_id,
                            )
                        });
                        c_stage < 2 || pilot_is_same_pirate
                    } else if is_insurgency_enforcer {
                        let s_stage = *cached_s_stage.get_or_insert_with(|| {
                            crate::services::security_mgr::insurgency_system_suppression_stage(
                                ctx,
                                solar_system_id,
                            )
                        });
                        s_stage < 3 || !owner.pilot_is_pirate
                    } else if is_ci_sentry {
                        !owner.pilot_is_pirate && !owner.has_criminal_flag
                    } else if is_ambush_tackler {
                        pilot_is_same_pirate
                    } else if is_ore_defender {
                        !owner.pilot_is_pirate
                    } else if is_ore_rorqual {
                        true
                    } else if is_outpost_defender {
                        if npc.faction_id == 500_018 { !owner.pilot_is_pirate } else { pilot_is_same_pirate }
                    } else if is_zarzakh_gun {
                        !owner.has_criminal_flag && !owner.has_suspect_flag && !owner.has_disapproval_flag
                    } else if is_zarzakh_patrol {
                        owner.pilot_is_pirate && !owner.has_disapproval_flag
                    } else if is_navy {
                        standing > -5.0 && !is_criminal
                    } else if is_police {
                        !is_criminal && standing > -5.0
                    } else if is_edencom {
                        standing >= 0.0 && !is_criminal
                    } else {
                        standing >= 5.0 && !is_criminal
                    };
                    if should_skip
                        && !owner_already_threat
                        && !npc.aggression_threat.contains_key(&cand_id)
                        && !npc.threat_table.contains_key(&cand_id)
                    {
                        continue;
                    }
                } else if !npc.aggression_threat.contains_key(&cand_id) && !npc.threat_table.contains_key(&cand_id) {
                    continue;
                }

                // DERIVED: threat weighting combines hull class/drone base, signature radius (attr 552), proximity, DPS, EWAR (1.5x), and logistics (1.3x).
                let sig_weight = (cand.sig_radius / 40.0).clamp(0.5, 5.0) * 10.0;
                let role_mult = if cand.has_ewar { 1.5 } else if cand.has_logi { 1.3 } else { 1.0 };
                let (drone_sig, drone_dps, drone_attacking) = if cand.is_player_drone {
                    let attacking = cand.drone_state.is_some_and(|(act, tid, _)| {
                        act == 1 && (tid.is_none() || tid == Some(npc.ball_id))
                    });
                    let d_dps = if attacking {
                        cand.drone_state.map(|(_, _, dps)| dps).unwrap_or(15.0)
                    } else {
                        0.0
                    };
                    (cand.drone_sig, d_dps, attacking)
                } else {
                    (0.0, 0.0, false)
                };
                let base_threat = if cand.is_capital {
                    if npc.archetype == "Sleeper" { 1_000.0 } else { (200.0 + sig_weight) * role_mult }
                } else if cand.is_player_drone {
                    let sig_bonus = (drone_sig / 25.0).clamp(0.5, 4.0) * 5.0;
                    if drone_attacking {
                        let prox_bonus = (10_000.0 / dist.max(500.0)).clamp(0.0, 20.0);
                        let dps_bonus = (drone_dps / 10.0).clamp(0.0, 25.0);
                        (140.0 + prox_bonus + sig_bonus + dps_bonus) * role_mult
                    } else {
                        (55.0 + sig_bonus * 0.5) * role_mult
                    }
                } else {
                    (100.0 + sig_weight) * role_mult
                };
                let entry = npc.aggression_threat.entry(cand_id).or_insert(base_threat);
                if cand.is_player_drone && !drone_attacking {
                    *entry = base_threat;
                } else if *entry < base_threat {
                    *entry = base_threat;
                }
            }
        }

        npc.threat_table = npc.aggression_threat.clone();

        // Pick highest-threat valid target on grid (breaking equal-threat ties deterministically by lowest ball ID)
        let next_target = npc
            .aggression_threat
            .iter()
            .max_by(|a, b| {
                a.1.partial_cmp(b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.0.cmp(a.0))
            })
            .map(|(&tid, _)| tid);
        if npc.target_id != next_target {
            npc.entropic_cycles = 0;
        }
        npc.target_id = next_target;

        let Some(target_id) = npc.target_id else {
            if npc != orig_npc {
                updated_npcs.push((nid, npc));
            }
            continue;
        };
        let Some(&target_ball) = lite_balls.get(&target_id) else {
            npc.target_id = None;
            if npc != orig_npc {
                updated_npcs.push((nid, npc));
            }
            continue;
        };
        let dist = npc_ball.position.distance(target_ball.position);

        // 3. Apply NPC EWAR & 4. Archetype Weapon Application synchronously on Ballpark
        let mut destroyed_in_tick: Vec<(i64, i64, eve_destiny::Ball)> = Vec::new();
        let did_aggress = {
            let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
            let mut st_guard = sys_handle.actor_state.write().unwrap_or_else(|e| e.into_inner());
            let st = &mut *st_guard;
            if npc.web_speed_factor < 0.0
                && dist <= npc.web_range
                && !bp.combat_states.get(&target_id).is_some_and(|tcs| tcs.is_web_immune)
            {
                let mult = (1.0 + npc.web_speed_factor / 100.0).clamp(0.1, 1.0);
                let _ = bp.set_speed_fraction(target_id, (target_ball.speed_fraction.max(1.0)) * mult);
            }
            let can_scram = if npc.archetype == "Sleeper" {
                npc.sleeper_wh_class >= 3
            } else {
                true
            };
            if can_scram && npc.scram_strength > 0.0 && dist <= npc.scram_range {
                active_npc_scram_targets.insert(target_id);
                st.base_scramble_strengths.insert(target_id, npc.scram_strength as i32);
                if let Some(tcs) = bp.combat_states.get_mut(&target_id) {
                    tcs.warp_scramble_strength =
                        tcs.warp_scramble_strength.max(npc.scram_strength as i32);
                    tcs.is_mwd_mjd_disabled = true;
                }
                let deact_guard = sys_handle
                    .module_deactivation_hook
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                crate::system_actor::reconcile_tackle_and_bubbles(
                    &mut bp,
                    &mut st.active_module_cycles,
                    &st.base_scramble_strengths,
                    &st.base_core_strengths,
                    &st.db_opt,
                    deact_guard.as_ref(),
                );
            }
            if npc.neut_amount > 0.0 && dist <= npc.optimal_range {
                if let Some(tcs) = bp.combat_states.get_mut(&target_id) {
                    tcs.capacitor = (tcs.capacitor - npc.neut_amount).max(0.0);
                }
            }

            let mut cycle_dmg = npc.damage_vector;
            if npc.archetype == "Drifter" && npc.drifter_overshield_hp > 0.0 {
                if let Some(ncs) = bp.combat_states.get(&nid) {
                    let lost = (ncs.max_shield_hp - ncs.shield_hp).max(0.0)
                        + (ncs.max_armor_hp - ncs.armor_hp).max(0.0)
                        + (ncs.max_structure_hp - ncs.structure_hp).max(0.0);
                    if lost >= npc.drifter_overshield_hp {
                        npc.drifter_overshield_hp = 0.0;
                    }
                }
            }
            if npc.archetype == "Triglavian" {
                let mult = 1.0 + (npc.entropic_cycles as f64 * 0.10).min(1.50);
                cycle_dmg = cycle_dmg.scaled(mult);
                npc.entropic_cycles = npc.entropic_cycles.saturating_add(1);
            } else if npc.archetype == "Drifter"
                && ctx.sde.types.get(&npc.type_id).is_none_or(|t| t.group_id != 1956)
                && npc.drifter_overshield_hp <= 0.0
                && !npc.drifter_doomsday_fired
            {
                npc.drifter_doomsday_fired = true;
                cycle_dmg = DamageVector::new(2_500.0, 2_500.0, 2_500.0, 2_500.0);
            } else if npc.archetype.eq_ignore_ascii_case("Edencom") {
                let candidates: Vec<(i64, DVec3)> = lite_balls
                    .iter()
                    .filter(|(&other_id, _)| {
                        other_id != nid
                            && other_id != target_id
                            && !destroyed_in_call.contains(&other_id)
                            && (npc.threat_table.contains_key(&other_id)
                                || is_candidate_hostile_to_edencom(ctx, &npc, other_id, target_id))
                    })
                    .map(|(&other_id, other_ball)| (other_id, other_ball.position))
                    .collect();
                let hits = crate::formulas::vorton_arc_targets_with_cap(
                    target_id,
                    target_ball.position,
                    &candidates,
                    10_000.0,
                    5,
                );
                for arc_id in hits.into_iter().skip(1) {
                    if let Some((_, true, Some(dead_ball))) = bp.apply_damage_to_ball(arc_id, npc.damage_vector.scaled(0.5)) {
                        destroyed_in_tick.push((nid, arc_id, dead_ball));
                    }
                }
            } else if npc.archetype == "Sleeper" {
                let is_cap = cand_by_id
                    .get(&target_id)
                    .map(|c| c.is_capital)
                    .unwrap_or_else(|| {
                        ctx.db.with_snapshot(|snap| {
                            snap.items.get(&target_id).and_then(|i| {
                                ctx.sde.types.get(&i.type_id).map(|t| matches!(t.group_id, 485 | 547 | 1538 | 30 | 659))
                            }).unwrap_or(false)
                        })
                    });
                if is_cap {
                    cycle_dmg = cycle_dmg.scaled(2.5);
                }
            } else if npc.archetype == "ZarzakhGateGun" {
                apply_zarzakh_gate_gun_ewar(&ctx.sde, npc.type_id, &mut bp, target_id, target_ball.radius, dist);
            } else if npc.archetype == "CounterInsurgencySentry" || npc.type_id == 79_507 {
                crate::services::security_mgr::apply_counter_insurgency_sentry_ewar(&ctx.sde, &mut bp, target_id, target_ball.radius, dist);
            }
            let can_fire = if npc.archetype == "SleeperSentry" {
                let interval_s = crate::combat::npc_ai::compute_sleeper_sentry_interval_s(ctx, npc.type_id);
                dist <= npc.optimal_range
                    && crate::services::wormhole_pve_escalations::can_sentry_fire_and_record(
                        ctx, nid, interval_s, st.sim_elapsed_secs,
                    )
            } else {
                true
            };
            if can_fire {
                if let Some((_, true, Some(dead_ball))) = bp.apply_damage_to_ball(target_id, cycle_dmg) {
                    destroyed_in_tick.push((nid, target_id, dead_ball));
                }
            }
            crate::system_actor::sync_ball_positions(&bp, &sys_handle.ball_positions, &sys_handle.scrambled_balls);
            can_fire || active_npc_scram_targets.contains(&target_id)
        };
        if !destroyed_in_tick.is_empty() {
            for &(_, did, _) in &destroyed_in_tick {
                destroyed_in_call.insert(did);
            }
            crate::combat::npc_ai::handle_npc_ai_ball_destructions(ctx, solar_system_id, destroyed_in_tick);
        }
        if did_aggress {
            let may_be_player_ship = cand_by_id
                .get(&target_id)
                .and_then(|c| c.owner.as_ref())
                .is_none_or(|o| o.is_active_player_ship);
            if may_be_player_ship {
                let now_u = ctx.now_unix();
                let min_fresh = now_u.saturating_add(crate::sp6_lifecycle::NPC_TIMER_SECS.saturating_sub(5));
                if let Some(target_char_id) = ctx.db.with_snapshot(|snap| {
                    let cid = snap.characters.values().find(|c| c.active_ship_id == target_id).map(|c| c.character_id)?;
                    if snap.sp6_lifecycle.disconnected_ships.get(&cid).is_some_and(|d| !d.removed_from_space) {
                        return None;
                    }
                    let sp_u = snap.sp6_lifecycle.npc_timers_until.get(&cid).copied().unwrap_or(0);
                    (snap.sp6_lifecycle.safe_logoffs.contains_key(&cid) || sp_u < min_fresh).then_some(cid)
                }) {
                    crate::sp6_lifecycle::record_npc_aggression_timer(ctx, target_char_id, now_u);
                }
            }
        }

        if npc != orig_npc {
            updated_npcs.push((nid, npc));
        }
    }

    if prev_npc_scram_targets
        .iter()
        .any(|tid| !active_npc_scram_targets.contains(tid))
    {
        let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let mut st_guard = sys_handle.actor_state.write().unwrap_or_else(|e| e.into_inner());
        let st = &mut *st_guard;
        for tid in prev_npc_scram_targets {
            if !active_npc_scram_targets.contains(&tid) {
                st.base_scramble_strengths.remove(&tid);
            }
        }
        let deact_guard = sys_handle
            .module_deactivation_hook
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        crate::system_actor::reconcile_tackle_and_bubbles(
            &mut bp,
            &mut st.active_module_cycles,
            &st.base_scramble_strengths,
            &st.base_core_strengths,
            &st.db_opt,
            deact_guard.as_ref(),
        );
        crate::system_actor::sync_ball_positions(
            &bp,
            &sys_handle.ball_positions,
            &sys_handle.scrambled_balls,
        );
    }

    let need_scram_write = !active_npc_scram_targets.is_empty() || has_scram;
    if !dead_npcs_to_remove.is_empty() || !updated_npcs.is_empty() || need_scram_write {
        with_mut_combat_ephemeral(ctx, |rt| {
            for dead_id in dead_npcs_to_remove {
                rt.npc_entities.remove(&dead_id);
            }
            for (nid, npc) in updated_npcs {
                if !destroyed_in_call.contains(&nid) && rt.npc_entities.contains_key(&nid) {
                    rt.npc_entities.insert(nid, npc);
                }
            }
            if !active_npc_scram_targets.is_empty() {
                rt.npc_scrambled_ships
                    .insert(solar_system_id, active_npc_scram_targets);
            } else if has_scram {
                rt.npc_scrambled_ships.remove(&solar_system_id);
            }
        });
    }
}

/// Accumulates threat against an NPC (`npc_ball_id`) from `attacker_ball_id` based on
/// weapon damage dealt, EWAR application (`1.5x` weight), remote logistics (`1.3x` weight),
/// and the attacker's signature radius (`attr 552`) (`feat:npc.npc-target-switching`).
pub fn record_npc_threat_event(
    ctx: &ServerContext,
    npc_ball_id: i64,
    attacker_ball_id: i64,
    damage_dealt: f64,
    is_ewar: bool,
    is_logistics: bool,
) {
    let sig_radius = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&attacker_ball_id)
            .and_then(|i| ctx.sde.types.get(&i.type_id))
            .and_then(|t| t.default_attributes.get(&552).copied())
            .unwrap_or(40.0)
    });
    // DERIVED: signature radius bonus relative to 40m baseline; EWAR 1.5x and logistics 1.3x multipliers.
    let sig_weight = (sig_radius / 40.0).clamp(0.5, 5.0) * 10.0;
    let role_mult = if is_ewar { 1.5 } else if is_logistics { 1.3 } else { 1.0 };
    let delta = (damage_dealt.max(if is_ewar || is_logistics { 120.0 } else { 0.0 }) + sig_weight) * role_mult;
    let mining_sys_opt = [npc_ball_id, npc_ball_id - 1_000_000, npc_ball_id - 2_000_000].into_iter()
        .find_map(|id| ctx.living_world.get_convoy(id))
        .filter(|c| c.archetype == crate::living_world::NpcArchetype::MiningOp).map(|c| c.current_system_id);
    let relay_sys_opt = with_mut_combat_ephemeral(ctx, |rt| {
        if let Some(sys_id) = mining_sys_opt {
            for npc in rt.npc_entities.values_mut().filter(|n| n.solar_system_id == sys_id && matches!(n.archetype.as_str(), "Navy" | "CaldariNavy" | "MinmatarNavy" | "AmarrNavy" | "GallenteNavy")) {
                let entry = npc.aggression_threat.entry(attacker_ball_id).or_insert(100.0);
                *entry += delta;
                npc.threat_table.insert(attacker_ball_id, *entry);
            }
        }
        if let Some(npc) = rt.npc_entities.get_mut(&npc_ball_id) {
            if npc.archetype == "TransmissionRelay" || npc.type_id == 41_589 {
                return Some(npc.solar_system_id);
            }
            if npc.archetype == "Drifter" && npc.drifter_overshield_hp > 0.0 && damage_dealt > 0.0 {
                npc.drifter_overshield_hp = (npc.drifter_overshield_hp - damage_dealt).max(0.0);
            }
            let entry = npc.aggression_threat.entry(attacker_ball_id).or_insert(100.0);
            *entry += delta;
            npc.threat_table.insert(attacker_ball_id, *entry);
        }
        None
    });
    if let Some(relay_sys) = relay_sys_opt {
        let _ = crate::services::wormhole_pve_escalations::on_decloaked_relay_attacked_sync(
            ctx,
            relay_sys,
            npc_ball_id,
        );
    }
}

pub async fn step_extended_npc_ai(ctx: &ServerContext, solar_system_id: u32) {
    step_extended_npc_ai_sync(ctx, solar_system_id);
}

/// Queues an NPC bounty kill into the 20-minute (`1,200s`) bounty tick accumulator (`feat:combat.bounty-payout-npc`)
/// and drops archetype-specific loot (Clone Soldier tags, Officer modules, Sleeper blue loot, Hauler minerals).
pub async fn destroy_specialized_npc_and_queue_bounty(
    ctx: &ServerContext,
    solar_system_id: u32,
    npc_ball_id: i64,
    killer_char_id: i64,
) -> (i64, f64, Vec<u32>) {
    destroy_specialized_npc_with_hint(ctx, solar_system_id, npc_ball_id, killer_char_id, None).await
}

pub async fn destroy_specialized_npc_with_hint(
    ctx: &ServerContext,
    solar_system_id: u32,
    npc_ball_id: i64,
    killer_char_id: i64,
    hint: Option<(u32, DVec3)>,
) -> (i64, f64, Vec<u32>) {
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let ball_opt = sys_handle.get_ball_snapshot(npc_ball_id).await.ok().flatten().map(|(_, b)| b);
    let pos = ball_opt
        .as_ref()
        .map(|b| b.position)
        .or_else(|| hint.map(|(_, p)| p))
        .unwrap_or(DVec3::ZERO);
    let _ = sys_handle.remove_ball(npc_ball_id).await;
    crate::services::fleet_social::fleet_social_finder_tactical::clear_destroyed_target_tag(ctx, npc_ball_id);

    let npc_opt = with_mut_combat_ephemeral(ctx, |rt| {
        rt.npc_entities
            .remove(&npc_ball_id)
            .or_else(|| rt.destroyed_npc_cache.remove(&npc_ball_id))
    });
    let convoy_opt = ctx.living_world.get_convoy(npc_ball_id);
    let mining_hauler_convoy = convoy_opt.is_none().then(|| ctx.living_world.get_mining_hauler_convoy(npc_ball_id)).flatten();
    let had_tracked_npc = npc_opt.is_some() || convoy_opt.is_some() || mining_hauler_convoy.is_some();
    let sde_bounty = |tid: u32| -> f64 {
        ctx.sde.types.get(&tid).and_then(|t| t.default_attributes.get(&481).copied()).unwrap_or(0.0)
    };
    let (archetype, bounty_isk, tag_drop_opt, npc_type_id, faction_id, custom_loot) = match npc_opt {
        Some(n) => (n.archetype, n.bounty_isk, n.tag_drop_type_id, Some(n.type_id), n.faction_id, n.custom_cargo_loot),
        None => {
            if let Some(ref conv) = convoy_opt {
                ("Hauler".to_string(), sde_bounty(conv.ship_type_id), None, Some(conv.ship_type_id), 500_001, Some((conv.cargo_type_id, conv.cargo_quantity as i64)))
            } else if mining_hauler_convoy.is_some() {
                let tid = ball_opt.as_ref().map(|b| b.type_id).or_else(|| hint.map(|(t, _)| t)).unwrap_or(648);
                let cargo = ctx.db.with_mut_snapshot(|snap| {
                    let found = snap.items.iter().find(|(_, i)| i.location_id == npc_ball_id && i.flag == InventoryFlag::Cargo && i.quantity > 0).map(|(&id, i)| (id, i.type_id, i.quantity as i64));
                    if let Some((id, _, _)) = found { snap.items.remove(&id); }
                    found.map(|(_, t, q)| (t, q))
                });
                ("Hauler".to_string(), sde_bounty(tid), None, Some(tid), 500_001, cargo)
            } else {
                let tid = ball_opt
                    .as_ref()
                    .map(|b| b.type_id)
                    .or_else(|| hint.map(|(t, _)| t))
                    .or_else(|| ctx.db.with_snapshot(|snap| snap.items.get(&npc_ball_id).map(|i| i.type_id)));
                ("BeltRat".to_string(), tid.map(sde_bounty).unwrap_or(0.0), None, tid, 0, None)
            }
        }
    };
    let effective_custom_loot = custom_loot.or_else(|| {
        convoy_opt.as_ref().map(|c| (c.cargo_type_id, c.cargo_quantity as i64))
    });
    if npc_type_id == Some(41_589) || archetype == "TransmissionRelay" {
        let _ = crate::services::wormhole_pve_escalations::on_decloaked_relay_attacked_sync(
            ctx,
            solar_system_id,
            npc_ball_id,
        );
    }
    if convoy_opt.is_some() {
        ctx.living_world.mark_convoy_destroyed(npc_ball_id);
    }
    if let Some(tid) = npc_type_id {
        crate::services::sov_gameplay::on_space_entity_destroyed_ext(
            ctx,
            solar_system_id,
            (killer_char_id > 0).then_some(killer_char_id),
            npc_ball_id,
            tid,
            had_tracked_npc,
        );
    }
    if archetype == "CloneSoldier" {
        let until = ctx.now_unix().saturating_add(1_200);
        with_mut_combat_ephemeral(ctx, |rt| {
            rt.clone_soldier_cooldown_until.insert(solar_system_id, until);
        });
    }

    let effective_bounty = if solar_system_id >= 31_000_000 || archetype == "Sleeper" {
        0.0
    } else {
        let inc_mult = crate::services::pve_special_sites::get_incursion_effects_for_system(ctx, solar_system_id)
            .map(|e| (1.0 - e.bounty_penalty).clamp(0.0, 1.0))
            .unwrap_or(1.0);
        bounty_isk * inc_mult
    };

    if npc_type_id == Some(crate::services::pve_fob_observatory::WORLD_ARK_BOSS_TYPE_ID) {
        crate::services::pve_fob_observatory::on_world_ark_boss_killed(ctx, solar_system_id, killer_char_id).await;
    }

    crate::combat::crimewatch::credit_specialized_npc_bounty_and_sec_status(
        ctx,
        solar_system_id,
        npc_ball_id,
        killer_char_id,
        effective_bounty,
    );
    if killer_char_id > 0 { crate::services::social_corp_fleet::social_corp_projects::record_server_project_activity(ctx, killer_char_id, &["DamageNPCShips", "DestroyNPCShips"], 1, Some(solar_system_id as i64)); }

    let wreck_type_id = if archetype == "Sleeper" {
        30495 // Sleeper Large Intermediate Wreck (`attr 901 = -15.0`)
    } else if archetype == "Drifter" {
        37531 // Drifter Response Battleship Wreck (`attr 901 = -20.0`)
    } else if archetype == "Hauler" {
        npc_type_id
            .and_then(|tid| ctx.sde.resolve_wreck_type_id(tid, None))
            .unwrap_or(26557)
    } else {
        npc_type_id
            .and_then(|tid| ctx.sde.resolve_wreck_type_id(tid, None))
            .unwrap_or(26473)
    };

    let mut dropped_types = Vec::new();
    if let Some(tag_tid) = tag_drop_opt {
        dropped_types.push(tag_tid);
    }
    if let Some((custom_tid, _)) = effective_custom_loot {
        if !dropped_types.contains(&custom_tid) {
            dropped_types.push(custom_tid);
        }
    }

    // Query SDE type materials and dogma attributes
    if let Some(tid) = npc_type_id {
        if let Some(mats) = ctx.sde.type_materials.get(&tid) {
            for m in mats {
                if !dropped_types.contains(&m.material_type_id) {
                    dropped_types.push(m.material_type_id);
                }
            }
        }
    }

    let sde_type = npc_type_id.and_then(|tid| ctx.sde.types.get(&tid));
    let effective_faction = sde_type
        .and_then(|t| t.faction_id)
        .or(if faction_id > 0 { Some(faction_id) } else { None });

    // Derive salvage and loot dynamically from SDE queries (ctx.sde.types, ctx.sde.groups, ctx.sde.npc_factions)
    match archetype.as_str() {
        "PochvenHypnosianSleeper" => {
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(_, t)| t.name == "Interstellar Navigation Logs") {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
        "Drifter" if npc_type_id == Some(37_473) => {
            for &tid in &[30_747_u32, 30_744_u32, 30_259_u32] {
                if ctx.sde.types.contains_key(&tid) && !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
        "Sleeper" => {
            let comp_gid = ctx
                .sde
                .groups
                .iter()
                .find(|(_, g)| g.name == "Sleeper Components")
                .map(|(&gid, _)| gid)
                .unwrap_or(880);
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(&id, t)| {
                t.group_id == comp_gid && (id == 30744 || t.name == "Neural Network Analyzer")
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
            let anc_gid = ctx
                .sde
                .groups
                .iter()
                .find(|(_, g)| g.name == "Ancient Salvage")
                .map(|(&gid, _)| gid)
                .unwrap_or(966);
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(&id, t)| {
                t.group_id == anc_gid && (id == 30259 || t.name == "Melted Nanoribbons")
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
        "Officer" | "MorduCommander" => {
            if let Some(tid) = npc_type_id
                .and_then(|ntid| crate::services::combat_crimewatch::combat_npc_encounters::resolve_officer_or_commander_sde_drop(ctx, ntid))
                .map(|(tid, _)| tid)
                .or_else(|| {
                    ctx.sde
                        .types
                        .iter()
                        .find(|(&id, t)| id == 14254 || (t.group_id == 52 && t.name.contains("Cormack")))
                        .map(|(&tid, _)| tid)
                })
            {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
        "Hauler" => {
            if let Some(ref conv) = convoy_opt {
                if !dropped_types.contains(&conv.cargo_type_id) { dropped_types.push(conv.cargo_type_id); }
            } else if let Some((custom_tid, _)) = custom_loot {
                if !dropped_types.contains(&custom_tid) { dropped_types.push(custom_tid); }
            }
            for tid in [34u32, 35, 36, 3828] {
                if !dropped_types.contains(&tid) { dropped_types.push(tid); }
            }
        }
        "RogueDrone" => {
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(&id, t)| {
                id == 11732 || t.name.contains("Drone Alloy") || t.name == "Sheen Compound"
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
        "Triglavian" => {
            let data_gid = ctx
                .sde
                .groups
                .iter()
                .find(|(_, g)| g.name == "Triglavian Data")
                .map(|(&gid, _)| gid)
                .unwrap_or(1995);
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(&id, t)| {
                id == 48121 || t.name == "Triglavian Survey Database" || (t.group_id == data_gid && t.name.contains("Survey Database"))
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
            let mat_gid = ctx
                .sde
                .groups
                .iter()
                .find(|(_, g)| g.name == "Abyssal Materials")
                .map(|(&gid, _)| gid)
                .unwrap_or(1996);
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(&id, t)| {
                id == 48112 || t.name == "Zero-Point Condensate" || (t.group_id == mat_gid && t.name.contains("Zero-Point"))
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
        "EDENCOM" | "Edencom" => {
            let salvage_gid = ctx
                .sde
                .groups
                .iter()
                .find(|(_, g)| g.name == "Salvaged Materials")
                .map(|(&gid, _)| gid)
                .unwrap_or(754);
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(_, t)| {
                t.group_id == salvage_gid && t.name == "Alloyed Tritanium Bar"
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
        _ => {
            let salvage_gid = ctx
                .sde
                .groups
                .iter()
                .find(|(_, g)| g.name == "Salvaged Materials")
                .map(|(&gid, _)| gid)
                .unwrap_or(754);
            let faction_name = effective_faction
                .and_then(|fid| ctx.sde.npc_factions.get(&fid).map(|f| f.name.as_str()))
                .unwrap_or("");
            let salvage_name = if faction_name.contains("Caldari") || faction_name.contains("Guristas") {
                "Alloyed Tritanium Bar"
            } else if faction_name.contains("Minmatar") || faction_name.contains("Angel") {
                "Armor Plates"
            } else if faction_name.contains("Amarr") || faction_name.contains("Blood") || faction_name.contains("Sansha") {
                "Burned Logic Circuit"
            } else if faction_name.contains("Gallente") || faction_name.contains("Serpentis") {
                "Artificial Neural Network"
            } else {
                "Metal Scraps"
            };
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(_, t)| {
                (t.group_id == salvage_gid && t.name == salvage_name) || t.name == salvage_name
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
    }

    let now_unix = ctx.now_unix();
    let wreck_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let wid = snap.next_entity_id;
        crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, wid, now_unix, true);
        snap.items.insert(
            wid,
            InventoryItemRecord {
                item_id: wid,
                type_id: wreck_type_id,
                owner_id: killer_char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions
            .insert(wid, [pos.x, pos.y, pos.z]);
        for &ltid in &dropped_types {
            snap.next_entity_id += 1;
            let lid = snap.next_entity_id;
            let qty = if let Some((custom_tid, custom_qty)) = effective_custom_loot {
                if custom_tid == ltid {
                    (custom_qty as i32).max(1)
                } else if ltid == 34 || ltid == 35 {
                    50_000
                } else {
                    1
                }
            } else if let Some(ref conv) = convoy_opt {
                if conv.cargo_type_id == ltid {
                    conv.cargo_quantity.max(1)
                } else if ltid == 34 || ltid == 35 {
                    50_000
                } else {
                    1
                }
            } else if ltid == 34 || ltid == 35 {
                50_000
            } else {
                1
            };
            let is_bpc = matches!(ltid, 33817 | 33819 | 33821);
            snap.items.insert(
                lid,
                InventoryItemRecord {
                    item_id: lid,
                    type_id: ltid,
                    owner_id: killer_char_id,
                    location_id: wid,
                    flag: InventoryFlag::Cargo,
                    quantity: if is_bpc { 1 } else { qty },
                    singleton: is_bpc,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: is_bpc,
                    blueprint_runs_remaining: is_bpc.then_some(1),
                },
            );
        }
        wid
    });

    let wreck_sde = ctx.sde.types.get(&wreck_type_id);
    let _ = sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: wreck_id,
                type_id: wreck_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(wreck_type_id),
            eve_destiny::SlimItemMeta {
                group_id: wreck_sde.map(|t| t.group_id).unwrap_or(186),
                category_id: wreck_sde.map(|t| t.category_id).unwrap_or(2),
                owner_id: killer_char_id,
                char_id: None,
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                name: wreck_sde.map(|t| t.name.clone()).filter(|n| !n.is_empty()),
                ..Default::default()
            },
        )
        .await;
    crate::services::combat_crimewatch::combat_wrecks_killmails::register_container_lifetime(
        ctx,
        wreck_id,
        solar_system_id,
        wreck_type_id,
        killer_char_id,
        now_unix,
        false,
    );

    // Check if `npc_ball_id` was an encounter wave trigger (`feat:npc.npc-spawn-encounter`)
    crate::services::space_mining_harvest::on_wormhole_ore_guard_destroyed_db(&ctx.db, npc_ball_id);
    crate::services::combat_crimewatch::combat_npc_encounters::step_encounter_wave_on_kill(ctx, solar_system_id, npc_ball_id, pos).await;

    // PvE exploration sites & escalations: progress waves, clear sites, roll escalations (`feat:pve.escalations`, `feat:pve.expeditions`)
    crate::services::pve_exploration::handle_pve_site_npc_destruction(ctx, solar_system_id, npc_ball_id, killer_char_id).await;

    // Multi-room dungeon runtime triggers (`OnKill`, gate unlocks, wave spawns, completion) (`feat:pve.dungeon-system`)
    crate::services::pve_dungeon_runtime::on_dungeon_npc_killed(ctx, solar_system_id, npc_ball_id, killer_char_id).await;

    crate::services::combat_crimewatch::combat_npc_encounters::record_npc_kill_progression(ctx, killer_char_id, npc_type_id);

    (wreck_id, effective_bounty, dropped_types)
}

fn apply_zarzakh_gate_gun_ewar(
    sde: &eve_sde::SdeRegistry,
    npc_type_id: u32,
    bp: &mut eve_destiny::Ballpark,
    target_id: i64,
    target_radius: f64,
    dist: f64,
) {
    let gun_type = sde.types.get(&npc_type_id).or_else(|| sde.types.get(&79_183));
    let attr = |aid: u16, def: f64| {
        gun_type
            .and_then(|t| t.default_attributes.get(&aid).copied())
            .unwrap_or(def)
    };
    let tp_range = attr(2524, 250_000.0);
    let tp_sig_pct = attr(554, 50.0);
    let ecm_range = attr(2532, 75_000.0);
    let ecm_dur_s = (attr(2822, 15_000.0) / 1_000.0).max(5.0);
    if dist <= tp_range {
        if let Some(t_cs) = bp.combat_states.get_mut(&target_id) {
            if t_cs.base_signature_radius.is_none() {
                t_cs.base_signature_radius = Some(if t_cs.signature_radius > 0.0 {
                    t_cs.signature_radius
                } else {
                    target_radius.max(35.0)
                });
            }
            t_cs.tp_sig_multiplier = Some(1.0 + tp_sig_pct / 100.0);
        }
        bp.recompute_ball_attributes(target_id);
    }
    if dist <= ecm_range && ecm_dur_s > 0.0 {
        bp.apply_ecm_jam(target_id, ecm_dur_s);
        bp.clear_target_locks(target_id);
    }
}

/// Ensures Zarzakh gate guns and Deathless / Outer Zarzakh pirate patrol fleets are present
/// at Zarzakh (`30_100_000`) gates/stations and at Zarzakh-linked gates and stations in adjacent k-space systems.
pub async fn ensure_zarzakh_patrols_and_gate_guns(ctx: &ServerContext, solar_system_id: u32) -> Vec<i64> {
    const ZARZAKH_SYSTEM_ID: u32 = 30_100_000;
    let mut specs: Vec<(u32, &'static str, u32, DVec3)> = Vec::new();
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);

    let existing_anchors: Vec<(String, DVec3)> =
        crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.npc_entities
                .values()
                .filter(|n| {
                    n.solar_system_id == solar_system_id
                        && matches!(
                            n.archetype.as_str(),
                            "ZarzakhGateGun" | "ZarzakhPatrol" | "OuterZarzakhPatrol"
                        )
                })
                .filter_map(|n| {
                    sys_handle
                        .get_ball_position_sync(n.ball_id)
                        .map(|pos| (n.archetype.to_string(), pos))
                })
                .collect()
        });

    let has_nearby = |arch: &str, center: DVec3| {
        existing_anchors
            .iter()
            .any(|(existing_arch, pos)| existing_arch == arch && pos.distance(center) <= 25_000.0)
    };

    if solar_system_id == ZARZAKH_SYSTEM_ID {
        let mut gate_positions: Vec<DVec3> = ctx
            .sde
            .stargates()
            .values()
            .filter(|g| g.from_solar_system_id == ZARZAKH_SYSTEM_ID)
            .map(|g| g.position)
            .collect();
        gate_positions.sort_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.z.total_cmp(&b.z)));
        for pos in gate_positions {
            if !has_nearby("ZarzakhGateGun", pos) {
                specs.push((79_183, "ZarzakhGateGun", 0, pos + DVec3::new(3_500.0, 0.0, 0.0)));
            }
            if !has_nearby("ZarzakhPatrol", pos) {
                specs.push((78_891, "ZarzakhPatrol", 0, pos + DVec3::new(-3_500.0, 1_500.0, 0.0)));
                specs.push((78_896, "ZarzakhPatrol", 0, pos + DVec3::new(1_500.0, -3_500.0, 0.0)));
            }
        }
    } else {
        let mut outer_gates: Vec<DVec3> = ctx
            .sde
            .stargates()
            .values()
            .filter(|g| g.from_solar_system_id == solar_system_id && g.to_solar_system_id == ZARZAKH_SYSTEM_ID)
            .map(|g| g.position)
            .collect();
        if outer_gates.is_empty() {
            return Vec::new();
        }
        outer_gates.sort_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.z.total_cmp(&b.z)));
        for pos in outer_gates {
            if !has_nearby("OuterZarzakhPatrol", pos) {
                specs.push((80_448, "OuterZarzakhPatrol", 0, pos + DVec3::new(3_500.0, 1_000.0, 0.0)));
                specs.push((80_451, "OuterZarzakhPatrol", 0, pos + DVec3::new(-3_500.0, -1_000.0, 0.0)));
            }
        }
        for st in ctx.sde.stations.values().filter(|s| s.solar_system_id == solar_system_id) {
            if !has_nearby("OuterZarzakhPatrol", st.position) {
                specs.push((80_449, "OuterZarzakhPatrol", 0, st.position + DVec3::new(4_500.0, 1_200.0, 0.0)));
                specs.push((80_452, "OuterZarzakhPatrol", 0, st.position + DVec3::new(-4_500.0, -1_200.0, 0.0)));
            }
        }
    }

    if specs.is_empty() {
        return Vec::new();
    }
    spawn_specialized_npc_batch(ctx, solar_system_id, &specs, |_| {}).await
}

pub use crate::services::combat_crimewatch::combat_npc_encounters::{
    create_custom_encounter_site, create_encounter_wave_site, flush_npc_bounty_tick,
    step_encounter_wave_on_kill,
};
#[cfg(test)]
#[path = "combat_npc_ai_tests.rs"] pub(crate) mod combat_npc_ai_tests;
#[cfg(test)]
#[path = "combat_npc_ai_edges_b4_tests.rs"]
mod combat_npc_ai_edges_b4_tests;

#[cfg(test)]
#[path = "combat_npc_ai_edges2_tests.rs"]
pub(crate) mod combat_npc_ai_edges2_tests;
