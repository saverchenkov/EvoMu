//! FW tactical objective structures (Supply Caches, Supply Depots, Listening Outposts, Propaganda Structures).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fw_services::{fw_opposing_faction, update_system_advantage};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldStateSnapshot};
use eve_destiny::{Ball, MovementMode};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use glam::DVec3;

/// Resolves `(type_id, shield_hp, armor_hp, hull_hp, radius)` for an FW Advantage site from `ctx.sde`.
pub(crate) fn resolve_supply_site_sde_spec(
    sde: &SdeRegistry,
    occupier_faction_id: i64,
    kind: &str,
    fallback_type_id: u32,
) -> (u32, f64, f64, f64, f64) {
    let type_id = match kind {
        "Propaganda" => match occupier_faction_id {
            500_003 => 73_225, // Amarr Propaganda Broadcast Structure
            500_002 => 73_226, // Minmatar Propaganda Broadcast Structure
            500_001 => 73_227, // Caldari Propaganda Broadcast Structure
            500_004 => 73_228, // Gallente Propaganda Broadcast Structure
            _ => 73_227,
        },
        "ListeningPost" => match occupier_faction_id {
            500_003 => 73_270, // Amarr Listening Outpost
            500_002 => 73_271, // Minmatar Listening Outpost
            500_001 => 73_272, // Caldari Listening Outpost
            500_004 => 73_273, // Gallente Listening Outpost
            _ => 73_272,
        },
        "SupplyDepot" => 354_772, // Supply Depot
        _ => 93_877,              // Warpath Supply Cache
    };
    let eff_tid = if sde.types.contains_key(&type_id) {
        type_id
    } else {
        fallback_type_id
    };
    let default_hp = if kind == "SupplyDepot" { 3_000.0 } else { 1_500.0 };
    let (shield_hp, armor_hp, hull_hp, radius) = sde
        .types
        .get(&eff_tid)
        .map(|t| {
            let s = t.default_attributes.get(&263).copied().filter(|&v| v > 0.0).unwrap_or(default_hp);
            let a = t.default_attributes.get(&265).copied().filter(|&v| v > 0.0).unwrap_or(default_hp);
            let h = t.default_attributes.get(&9).copied().filter(|&v| v > 0.0).unwrap_or(default_hp);
            let r = if t.radius > 0.0 { t.radius } else { 150.0 };
            (s, a, h, r)
        })
        .unwrap_or((default_hp, default_hp, default_hp, 150.0));
    (eff_tid, shield_hp, armor_hp, hull_hp, radius)
}

pub(crate) fn spawn_warzone_supply_sites_in_snap(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    sys_id: u32,
    occ_fac: i64,
    occ_corp: i64,
    fallback_type_id: u32,
    balls_to_spawn: &mut Vec<(i64, u32, [f64; 3], f64)>,
) {
    let supply_base =
        crate::services::space_destiny::space_docking_cloaking::pick_system_celestial_position(ctx, sys_id, 5).0;
    for (kind, pos_offset) in [
        ("SupplyCache", [0.0, 20_000.0, 0.0]),
        ("SupplyDepot", [5_000.0, -20_000.0, 0.0]),
        ("ListeningPost", [10_000.0, 25_000.0, 0.0]),
        ("Propaganda", [15_000.0, -25_000.0, 0.0]),
    ] {
        let pos = [
            supply_base[0] + pos_offset[0],
            supply_base[1] + pos_offset[1],
            supply_base[2] + pos_offset[2],
        ];
        let (site_tid, s_hp, a_hp, h_hp, radius) =
            resolve_supply_site_sde_spec(&ctx.sde, occ_fac, kind, fallback_type_id);
        let existing = snap
            .fw_runtime
            .supply_sites
            .iter()
            .find(|(&sid, (ssys, skind, _))| {
                *ssys == sys_id
                    && skind == kind
                    && snap.structures.get(&sid).is_some_and(|st| st.state != "Destroyed")
            })
            .map(|(&sid, _)| sid);
        let sid = if let Some(id) = existing {
            id
        } else {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.fw_runtime
                .supply_sites
                .insert(id, (sys_id, kind.to_string(), occ_fac));
            snap.structures.insert(
                id,
                UpwellStructureRecord {
                    structure_id: id,
                    type_id: site_tid,
                    owner_corp_id: occ_corp,
                    solar_system_id: sys_id,
                    name: format!("Militia {kind}"),
                    state: "ShieldVulnerable".into(),
                    position: pos,
                    quantum_core_installed: false,
                    fuel_blocks: 0,
                    shield_hp: s_hp,
                    armor_hp: a_hp,
                    hull_hp: h_hp,
                    reinforced_until_unix: 0,
                    last_damage_unix: 0,
                    last_damage_tick: 0,
                    self_destruct_at_unix: None,
                },
            );
            id
        };
        balls_to_spawn.push((sid, site_tid, pos, radius));
    }
}

fn supply_loot_type(faction_id: i64) -> u32 {
    match faction_id {
        500_003 => 15_607, // Imperial Navy Midshipman Insignia I
        500_002 => 15_625, // Republic Fleet Midshipman Insignia I
        500_001 => 15_596, // Caldari Navy Midshipman Insignia I
        500_004 => 15_587, // Federation Navy Midshipman Insignia I
        _ => 15_596,
    }
}

pub(crate) async fn apply_supply_site_damage(
    ctx: &ServerContext,
    attacker_char_id: i64,
    structure_id: i64,
    damage: f64,
    sys_id: u32,
    site_kind: String,
    site_fac: i64,
    now: u64,
) -> Result<PyRep, BridgeRpcError> {
    let outcome = ctx.db.try_with_mut_snapshot(|snap| {
        let att_fac = snap
            .characters
            .get(&attacker_char_id)
            .and_then(|c| c.war_faction_id)
            .ok_or_else(|| BridgeRpcError {
                code: "NotEnlistedInMilitia".into(),
                message: "Only militia-enlisted pilots can attack FW Supply Caches/Depots".into(),
            })?;
        if att_fac == site_fac {
            return Err(BridgeRpcError {
                code: "CannotAttackFriendlyStructure".into(),
                message: "Cannot attack your own militia's Supply Cache/Depot".into(),
            });
        }
        let st = snap.structures.get_mut(&structure_id).ok_or_else(|| BridgeRpcError {
            code: "StructureNotFound".into(),
            message: format!("Supply site {structure_id} not found"),
        })?;
        if st.state == "Destroyed" {
            return Err(BridgeRpcError {
                code: "StructureDestroyed".into(),
                message: format!("Supply site {structure_id} is already destroyed"),
            });
        }
        let pos = st.position;
        let mut rem = damage.max(0.0);
        for layer in [&mut st.shield_hp, &mut st.armor_hp, &mut st.hull_hp] {
            if rem > 0.0 && *layer > 0.0 {
                let take = rem.min(*layer);
                *layer -= take;
                rem -= take;
            }
        }
        if damage > 0.0 {
            st.last_damage_unix = now;
        }
        if st.hull_hp > 0.0 {
            return Ok((false, st.shield_hp, st.armor_hp, st.hull_hp, pos, att_fac, 0, Vec::new()));
        }
        st.state = "Destroyed".into();
        snap.fw_runtime.supply_sites.remove(&structure_id);
        snap.next_entity_id += 1;
        let can_id = snap.next_entity_id;
        snap.items.insert(
            can_id,
            InventoryItemRecord {
                item_id: can_id,
                type_id: 23,
                owner_id: attacker_char_id,
                location_id: i64::from(sys_id),
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(can_id, pos);
        snap.next_entity_id += 1;
        let loot_id = snap.next_entity_id;
        let is_depot = site_kind == "SupplyDepot";
        snap.items.insert(
            loot_id,
            InventoryItemRecord {
                item_id: loot_id,
                type_id: supply_loot_type(site_fac),
                owner_id: attacker_char_id,
                location_id: can_id,
                flag: InventoryFlag::Cargo,
                quantity: if is_depot { 5 } else { 2 },
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let supply_item_id = snap.next_entity_id;
        snap.items.insert(
            supply_item_id,
            InventoryItemRecord {
                item_id: supply_item_id,
                type_id: 88_103,
                owner_id: attacker_char_id,
                location_id: can_id,
                flag: InventoryFlag::Cargo,
                quantity: if is_depot { 10 } else { 5 },
                ..Default::default()
            },
        );
        Ok((true, 0.0, 0.0, 0.0, pos, att_fac, can_id, vec![loot_id, supply_item_id]))
    })?;
    let (destroyed, s_hp, a_hp, h_hp, pos, att_fac, can_id, dropped_ids) = outcome;
    if destroyed {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle.remove_ball(structure_id).await;
        let _ = sys_handle
            .spawn_ball(Ball {
                id: can_id,
                type_id: 23,
                position: DVec3::from_array(pos),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 2_500.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
        let adv_delta = match site_kind.as_str() {
            "SupplyDepot" | "ListeningPost" => 5.0,
            "Propaganda" => 4.0,
            _ => 2.0,
        };
        let occ_fac = ctx
            .db
            .with_snapshot(|s| s.fw_system_occupations.get(&sys_id).map(|o| o.occupier_faction_id))
            .unwrap_or(site_fac);
        let is_defender = att_fac == occ_fac || fw_opposing_faction(occ_fac) == Some(site_fac);
        let (occ_d, att_d) = if is_defender { (adv_delta, 0.0) } else { (0.0, adv_delta) };
        let (occ_adv, att_adv) = update_system_advantage(ctx, sys_id, occ_d, att_d);
        let (friendly_adv, enemy_adv) = if is_defender { (occ_adv, att_adv) } else { (att_adv, occ_adv) };
        let dropped_reps: Vec<PyRep> = dropped_ids.into_iter().map(PyRep::Integer).collect();
        return Ok(PyRep::keyval([
            ("structureID", PyRep::Integer(structure_id)),
            ("state", PyRep::String("Destroyed".into())),
            ("lootContainerID", PyRep::Integer(can_id)),
            ("droppedItemIDs", PyRep::List(dropped_reps)),
            ("friendlyAdvantage", PyRep::Real(friendly_adv)),
            ("enemyAdvantage", PyRep::Real(enemy_adv)),
        ]));
    }
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("state", PyRep::String("ShieldVulnerable".into())),
        ("shieldHP", PyRep::Real(s_hp)),
        ("armorHP", PyRep::Real(a_hp)),
        ("hullHP", PyRep::Real(h_hp)),
    ]))
}
