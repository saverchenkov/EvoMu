//! `pve_fob_observatory` — Blood Raider & Guristas Forward Operating Bases (FOB)
//! (`feat:pve.pirate-fob-forward-operating-base`) and Pochven Observatory Flashpoint & World Ark
//! (`feat:pve.triglavian-world-ark-observatory`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_incursion_sites::make_site_npc_ball;
use eve_db::{
    InventoryFlag, InventoryItemRecord as InventoryItem, IskCents, UpwellStructureRecord,
    WalletJournalEntry,
};
use eve_destiny::{CombatState, DVec3, MovementMode as BallMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub const FACTION_GURISTAS: i64 = 500_010;
pub const FACTION_BLOOD_RAIDER: i64 = 500_012;
pub const FACTION_TRIGLAVIAN: i64 = 500_026;
pub const CORP_DED: i64 = 1_000_137;
pub const POCHVEN_REGION_ID: u32 = 10_000_070;
pub const FOB_GURISTAS_TYPE_ID: u32 = 46_363; // Guristas Pirates Stronghold
pub const FOB_BLOOD_RAIDER_TYPE_ID: u32 = 46_364; // Blood Raiders Stronghold
pub const WORLD_ARK_BOSS_TYPE_ID: u32 = 52_361; // World Ark (Xordazh-class)
pub const OBSERVATORY_MAX_PAYOUT_PILOTS: usize = 15;
pub const OBSERVATORY_COLLECTIVE_ISK: f64 = 1_500_000_000.0;
pub const OBSERVATORY_COLLECTIVE_LP: i64 = 150_000;
pub const FOB_COLLECTIVE_BOUNTY_ISK: f64 = 300_000_000.0;
pub const OBSERVATORY_SITE_POS: DVec3 = DVec3::new(-300_000.0, 0.0, 300_000.0);

/// Spawns a Blood Raider (`500012`) or Guristas (`500010`) Forward Operating Base in a high/low-sec system
/// and launches an initial raiding fleet.
pub async fn spawn_pirate_fob(
    ctx: &ServerContext,
    sys_id: u32,
    pirate_faction_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    if !matches!(pirate_faction_id, FACTION_GURISTAS | FACTION_BLOOD_RAIDER) {
        return Err(BridgeRpcError {
            code: "InvalidPirateFaction".into(),
            message: "Pirate FOBs must belong to Blood Raider (500012) or Guristas (500010)".into(),
        });
    }
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5);
    if sys_sec <= 0.0 {
        return Err(BridgeRpcError {
            code: "InvalidSystemSecurity".into(),
            message: format!("Pirate FOBs only spawn in high/low-sec systems (sec={sys_sec:.2})"),
        });
    }

    let now_unix = ctx.now_unix_secs();
    let fob_id = 1_897_000_000 + i64::from(sys_id) * 100 + (pirate_faction_id % 100);
    let fob_type_id = if pirate_faction_id == FACTION_BLOOD_RAIDER {
        FOB_BLOOD_RAIDER_TYPE_ID
    } else {
        FOB_GURISTAS_TYPE_ID
    };
    let fob_pos = [200_000.0, 0.0, -200_000.0];

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            fob_id,
            UpwellStructureRecord {
                structure_id: fob_id,
                type_id: fob_type_id,
                owner_corp_id: pirate_faction_id,
                solar_system_id: sys_id,
                name: if pirate_faction_id == FACTION_BLOOD_RAIDER {
                    "Blood Raider Forward Operating Base".into()
                } else {
                    "Guristas Forward Operating Base".into()
                },
                position: fob_pos,
                state: "Online".into(),
                quantum_core_installed: true,
                fuel_blocks: 10_000,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_unix: 0,
                last_damage_tick: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.service_state_blobs.insert(
            format!("fob_faction:{fob_id}"),
            pirate_faction_id.to_string(),
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut fob_ball = make_site_npc_ball(fob_id, fob_type_id, DVec3::from_array(fob_pos));
    fob_ball.radius = 2_500.0;
    fob_ball.mass = 500_000_000.0;
    fob_ball.max_velocity = 0.0;
    fob_ball.mode = BallMode::Rigid;
    let mut fob_cs = CombatState::default();
    fob_cs.shield_hp = 500_000.0;
    fob_cs.max_shield_hp = 500_000.0;
    fob_cs.armor_hp = 500_000.0;
    fob_cs.max_armor_hp = 500_000.0;
    fob_cs.structure_hp = 500_000.0;
    fob_cs.max_structure_hp = 500_000.0;
    let _ = sys.spawn_ball_with_combat(fob_ball, fob_cs).await;

    // Launch 3 raiding fleet NPCs in the system (SDE Blood Raider / Guristas cruisers)
    let raider_type_id = if pirate_faction_id == FACTION_BLOOD_RAIDER {
        24_000 // Corpatis Bishop
    } else {
        16_980 // Pithum Killer
    };
    let mut raiding_ids = Vec::new();
    for idx in 1..=3_i64 {
        let raider_id = fob_id + idx;
        let _ = sys
            .spawn_ball_with_combat(
                make_site_npc_ball(
                    raider_id,
                    raider_type_id,
                    DVec3::from_array(fob_pos) + DVec3::new((idx as f64) * 5_000.0, 0.0, 0.0),
                ),
                ctx.build_type_combat_state(raider_type_id),
            )
            .await;
        raiding_ids.push(raider_id);
    }

    Ok(PyRep::keyval([
        ("fobID", PyRep::Integer(fob_id)),
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("pirateFactionID", PyRep::Integer(pirate_faction_id)),
        ("spawnedAtUnix", PyRep::Integer(now_unix as i64)),
        (
            "raidingFleetIDs",
            PyRep::List(raiding_ids.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}

/// Launches defending response ships scaled to attacking on-grid player fleet composition.
pub async fn trigger_fob_defense_response(
    ctx: &ServerContext,
    fob_id: i64,
    attacker_ship_ids: &[i64],
) -> Result<PyRep, BridgeRpcError> {
    let (sys_id, fob_pos, pirate_faction_id) = ctx
        .db
        .with_snapshot(|snap| {
            snap.structures.get(&fob_id).map(|s| {
                (
                    s.solar_system_id,
                    DVec3::from_array(s.position),
                    s.owner_corp_id,
                )
            })
        })
        .ok_or_else(|| BridgeRpcError {
            code: "FobNotFound".into(),
            message: format!("Pirate FOB {fob_id} not found"),
        })?;

    let fleet_weight: usize = ctx.db.with_snapshot(|snap| {
        attacker_ship_ids
            .iter()
            .map(|sid| {
                let group_id = snap
                    .items
                    .get(sid)
                    .and_then(|it| ctx.sde.types.get(&it.type_id))
                    .map(|t| t.group_id)
                    .unwrap_or(25);
                match group_id {
                    30 | 485 | 547 | 659 | 883 => 4, // Capital / Supercapital
                    27 | 898 | 900 => 3,             // Battleship / Marauder / Black Ops
                    26 | 358 | 419 | 832 | 906 => 2, // Cruiser / BC / Logistics / HAC
                    _ => 1,                          // Frigate / Destroyer
                }
            })
            .sum()
    });
    let defender_count = fleet_weight.clamp(2, 24);
    let defender_type_id = if pirate_faction_id == FACTION_BLOOD_RAIDER {
        16_934 // Corpus Prophet
    } else {
        16_984 // Pith Dismantler
    };
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut defender_ids = Vec::with_capacity(defender_count);
    for idx in 1..=(defender_count as i64) {
        let def_id = fob_id + 10 + idx;
        let _ = sys
            .spawn_ball_with_combat(
                make_site_npc_ball(
                    def_id,
                    defender_type_id,
                    fob_pos + DVec3::new(0.0, (idx as f64) * 1_500.0, 2_500.0),
                ),
                ctx.build_type_combat_state(defender_type_id),
            )
            .await;
        defender_ids.push(def_id);
    }

    Ok(PyRep::keyval([
        ("fobID", PyRep::Integer(fob_id)),
        ("attackerCount", PyRep::Integer(attacker_ship_ids.len() as i64)),
        ("fleetCompositionWeight", PyRep::Integer(fleet_weight as i64)),
        ("defenderCount", PyRep::Integer(defender_ids.len() as i64)),
        (
            "defenderIDs",
            PyRep::List(defender_ids.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}

/// Destroys a Pirate FOB, drops structure loot container in space, and pays collective bounties.
pub async fn destroy_pirate_fob_and_payout(
    ctx: &ServerContext,
    fob_id: i64,
    attacker_char_ids: &[i64],
) -> Result<PyRep, BridgeRpcError> {
    let (sys_id, fob_pos, pirate_faction_id) = ctx.db.with_mut_snapshot(|snap| {
        let st = snap
            .structures
            .get_mut(&fob_id)
            .ok_or_else(|| BridgeRpcError {
                code: "FobNotFound".into(),
                message: format!("Pirate FOB {fob_id} not found"),
            })?;
        if st.state == "Destroyed" {
            return Err(BridgeRpcError {
                code: "FobAlreadyDestroyed".into(),
                message: format!("Pirate FOB {fob_id} is already destroyed"),
            });
        }
        st.state = "Destroyed".into();
        st.hull_hp = 0.0;
        Ok((
            st.solar_system_id,
            DVec3::from_array(st.position),
            st.owner_corp_id,
        ))
    })?;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.remove_ball(fob_id).await;

    let loot_container_id = fob_id + 90;
    let mut cont_ball = make_site_npc_ball(loot_container_id, 23, fob_pos);
    cont_ball.max_velocity = 0.0;
    cont_ball.mode = BallMode::Rigid;
    let _ = sys.spawn_ball(cont_ball).await;

    let per_pilot_bounty: i64 = if attacker_char_ids.is_empty() {
        0
    } else {
        (FOB_COLLECTIVE_BOUNTY_ISK as i64) / (attacker_char_ids.len() as i64)
    };
    let faction_loot_type_id = if pirate_faction_id == FACTION_BLOOD_RAIDER {
        13_791 // Dark Blood Dual Heavy Pulse Laser
    } else {
        13_865 // Dread Guristas 125mm Railgun
    };

    ctx.db.with_mut_snapshot(|snap| {
        let loot_item_id = fob_id + 91;
        snap.items.insert(
            loot_item_id,
            InventoryItem {
                item_id: loot_item_id,
                type_id: faction_loot_type_id,
                owner_id: CORP_DED,
                location_id: loot_container_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        for &cid in attacker_char_ids {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = ch
                    .balance
                    .checked_add(IskCents::from_isk(per_pilot_bounty))
                    .unwrap_or(ch.balance);
                let new_bal = ch.balance;
                let entry_id = (snap.wallet_journal.len() as i64) + 1;
                snap.wallet_journal.push(WalletJournalEntry {
                    entry_id,
                    character_id: cid,
                    entry_type: "BountyPrizes".into(),
                    amount: IskCents::from_isk(per_pilot_bounty),
                    balance_after: new_bal,
                    reason: "Pirate FOB Destruction Bounty".into(),
                });
            }
        }
    });

    Ok(PyRep::keyval([
        ("fobID", PyRep::Integer(fob_id)),
        ("lootContainerID", PyRep::Integer(loot_container_id)),
        ("bountyPerPilot", PyRep::Integer(per_pilot_bounty)),
        ("totalBountyPaid", PyRep::Real(FOB_COLLECTIVE_BOUNTY_ISK)),
    ]))
}

/// Spawns a Pochven Observatory Flashpoint site with Triglavian defense waves and Xordazh World Ark boss.
pub async fn spawn_observatory_flashpoint(
    ctx: &ServerContext,
    sys_id: u32,
) -> Result<PyRep, BridgeRpcError> {
    let region_id = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.region_id)
        .unwrap_or(0);
    if region_id != POCHVEN_REGION_ID {
        return Err(BridgeRpcError {
            code: "NotInPochven".into(),
            message: format!(
                "Observatory Flashpoint sites only spawn in Pochven (region {POCHVEN_REGION_ID})"
            ),
        });
    }

    let site_id = 1_899_000_000 + i64::from(sys_id);
    let site_pos = OBSERVATORY_SITE_POS;
    let gate_id = site_id + 2;
    let gate_pos = site_pos + DVec3::new(-50_000.0, 0.0, 0.0);
    let sys = ctx.systems.get_or_spawn(sys_id);

    let mut gate_ball = make_site_npc_ball(gate_id, 17_831, gate_pos);
    gate_ball.max_velocity = 0.0;
    gate_ball.mode = BallMode::Rigid;
    let _ = sys.spawn_ball(gate_ball).await;

    let world_ark_id = site_id + 1;
    let mut boss_cs = CombatState::default();
    boss_cs.shield_hp = 200_000.0;
    boss_cs.max_shield_hp = 200_000.0;
    boss_cs.armor_hp = 400_000.0;
    boss_cs.max_armor_hp = 400_000.0;
    boss_cs.structure_hp = 200_000.0;
    boss_cs.max_structure_hp = 200_000.0;
    let _ = sys
        .spawn_ball_with_combat(
            make_site_npc_ball(world_ark_id, WORLD_ARK_BOSS_TYPE_ID, site_pos),
            boss_cs,
        )
        .await;

    let wave_types = [52_338_u32, 52_184_u32, 52_187_u32];
    let mut defense_wave_ids = Vec::with_capacity(wave_types.len());
    for (idx, &wtid) in wave_types.iter().enumerate() {
        let wid = site_id + 10 + (idx as i64);
        let _ = sys
            .spawn_ball_with_combat(
                make_site_npc_ball(
                    wid,
                    wtid,
                    site_pos + DVec3::new(((idx as f64) + 1.0) * 4_000.0, 1_500.0, 0.0),
                ),
                ctx.build_type_combat_state(wtid),
            )
            .await;
        defense_wave_ids.push(wid);
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            format!("observatory_site:{site_id}"),
            sys_id.to_string(),
        );
    });

    Ok(PyRep::keyval([
        ("siteID", PyRep::Integer(site_id)),
        ("gateID", PyRep::Integer(gate_id)),
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("worldArkBossID", PyRep::Integer(world_ark_id)),
        (
            "defenseWaveIDs",
            PyRep::List(defense_wave_ids.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}

/// Validates fleet composition (>= 3 pilots) and standing access (>= 0.0) for Observatory Flashpoint.
pub fn validate_observatory_access(
    ctx: &ServerContext,
    char_id: i64,
    fleet_member_ids: &[i64],
) -> Result<(), BridgeRpcError> {
    if fleet_member_ids.len() < 3 {
        return Err(BridgeRpcError {
            code: "InsufficientFleetComposition".into(),
            message: "Observatory Flashpoint requires a fleet of at least 3 pilots".into(),
        });
    }
    let trig_standing = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&FACTION_TRIGLAVIAN).copied())
            .unwrap_or(0.0)
    });
    if trig_standing < 0.0 {
        return Err(BridgeRpcError {
            code: "StandingAccessDenied".into(),
            message: format!("Triglavian standing {trig_standing:.2} is below required 0.0"),
        });
    }
    Ok(())
}

/// Completes an Observatory Flashpoint site and pays collective ISK and DED LP to up to 15 on-grid fleet members.
pub fn complete_observatory_flashpoint(
    ctx: &ServerContext,
    site_id: i64,
    fleet_member_ids: &[i64],
) -> Result<PyRep, BridgeRpcError> {
    let eligible: Vec<i64> = fleet_member_ids
        .iter()
        .copied()
        .take(OBSERVATORY_MAX_PAYOUT_PILOTS)
        .collect();
    if eligible.is_empty() {
        return Err(BridgeRpcError {
            code: "NoEligiblePilots".into(),
            message: "No eligible fleet members for Observatory Flashpoint payout".into(),
        });
    }

    let isk_each = (OBSERVATORY_COLLECTIVE_ISK as i64) / (eligible.len() as i64);
    let lp_each = OBSERVATORY_COLLECTIVE_LP / (eligible.len() as i64);

    ctx.db.with_mut_snapshot(|snap| {
        if snap
            .service_state_blobs
            .remove(&format!("observatory_site:{site_id}"))
            .is_none()
        {
            return Err(BridgeRpcError {
                code: "ObservatorySiteNotFound".into(),
                message: format!("Observatory Flashpoint site {site_id} does not exist"),
            });
        }
        snap.service_state_blobs
            .insert(format!("observatory_completed:{site_id}"), "1".into());
        for &cid in &eligible {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = ch
                    .balance
                    .checked_add(IskCents::from_isk(isk_each))
                    .unwrap_or(ch.balance);
                let new_bal = ch.balance;
                let entry_id = (snap.wallet_journal.len() as i64) + 1;
                snap.wallet_journal.push(WalletJournalEntry {
                    entry_id,
                    character_id: cid,
                    entry_type: "CorporateRewardPayout".into(),
                    amount: IskCents::from_isk(isk_each),
                    balance_after: new_bal,
                    reason: "Observatory Flashpoint Collective Payout".into(),
                });
                *ch.loyalty_points.entry(CORP_DED).or_insert(0) += lp_each;
            }
        }
        Ok(())
    })?;

    Ok(PyRep::keyval([
        ("siteID", PyRep::Integer(site_id)),
        ("paidPilotCount", PyRep::Integer(eligible.len() as i64)),
        ("iskPerPilot", PyRep::Integer(isk_each)),
        ("lpPerPilot", PyRep::Integer(lp_each)),
    ]))
}

/// Validates fleet composition (>= 3) and Triglavian standing (>= 0.0) when activating an Observatory acceleration gate.
pub async fn try_activate_observatory_gate(
    ctx: &ServerContext,
    session: &SessionState,
    sys_id: u32,
    gate_id: i64,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let site_id = 1_899_000_000 + i64::from(sys_id);
    if gate_id != site_id + 2 {
        return None;
    }
    let has_site = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .contains_key(&format!("observatory_site:{site_id}"))
    });
    if !has_site {
        return None;
    }
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let fleet_member_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        if let Some(fleet) = snap.fleets.values().find(|f| f.members.contains(&char_id)) {
            fleet.members.clone()
        } else {
            vec![char_id]
        }
    });
    if let Err(e) = validate_observatory_access(ctx, char_id, &fleet_member_ids) {
        return Some(Err(e));
    }
    if let Some(ship_id) = session.ship_id {
        let sys = ctx.systems.get_or_spawn(sys_id);
        let _ = sys
            .set_ball_position(ship_id, OBSERVATORY_SITE_POS + DVec3::new(0.0, 0.0, -5_000.0))
            .await;
    }
    Some(Ok(PyRep::keyval([
        ("siteID", PyRep::Integer(site_id)),
        ("accessGranted", PyRep::Bool(true)),
    ])))
}

/// Completes the Observatory Flashpoint when the World Ark boss (`52361`) is killed in the NPC death pipeline.
pub async fn on_world_ark_boss_killed(ctx: &ServerContext, sys_id: u32, killer_char_id: i64) {
    let site_id = 1_899_000_000 + i64::from(sys_id);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let all_balls = sys.list_balls().await.unwrap_or_default();
    let on_grid_ship_ids: std::collections::HashSet<i64> = all_balls
        .iter()
        .filter(|b| b.position.distance(OBSERVATORY_SITE_POS) <= 250_000.0)
        .map(|b| b.id)
        .collect();
    let fleet_member_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let is_on_site = |cid: &i64| {
            snap.characters.get(cid).is_some_and(|c| {
                c.solar_system_id == sys_id
                    && c.station_id.is_none()
                    && c.active_ship_id > 0
                    && on_grid_ship_ids.contains(&c.active_ship_id)
            })
        };
        if let Some(fleet) = snap
            .fleets
            .values()
            .find(|f| f.members.contains(&killer_char_id))
        {
            fleet
                .members
                .iter()
                .copied()
                .filter(is_on_site)
                .collect()
        } else {
            let mut ids: Vec<i64> = snap
                .characters
                .keys()
                .copied()
                .filter(is_on_site)
                .collect();
            ids.sort_unstable();
            ids
        }
    });
    if !fleet_member_ids.is_empty() {
        let _ = complete_observatory_flashpoint(ctx, site_id, &fleet_member_ids);
    }
}

/// Drives Pirate FOB and Pochven Observatory Flashpoint spawns from the living-world cluster tick.
pub async fn step_living_world_fob_and_observatory_spawns(ctx: &ServerContext) {
    let active_handles = ctx.systems.active_system_handles();
    for &(sys_id, _) in &active_handles {
        let has_piloted_ship = ctx.db.with_snapshot(|s| {
            s.characters.values().any(|c| {
                c.solar_system_id == sys_id
                    && c.station_id.is_none()
                    && c.structure_id.is_none()
                    && c.active_ship_id > 0
            })
        });
        if !has_piloted_ship {
            continue;
        }
        let Some(sys_info) = ctx.sde.solar_systems.get(&sys_id) else {
            continue;
        };
        if sys_info.security_status > 0.0 && sys_info.security_status < 0.8 {
            let sys_has_fob = ctx.db.with_snapshot(|snap| {
                snap.structures.values().any(|s| {
                    s.solar_system_id == sys_id
                        && matches!(s.type_id, FOB_GURISTAS_TYPE_ID | FOB_BLOOD_RAIDER_TYPE_ID)
                })
            });
            if !sys_has_fob {
                let faction = if sys_id % 2 == 1 {
                    FACTION_BLOOD_RAIDER
                } else {
                    FACTION_GURISTAS
                };
                let _ = spawn_pirate_fob(ctx, sys_id, faction).await;
            }
        }
        if sys_info.region_id == POCHVEN_REGION_ID {
            let site_id = 1_899_000_000 + i64::from(sys_id);
            let obs_exists_or_done = ctx.db.with_snapshot(|snap| {
                snap.service_state_blobs
                    .contains_key(&format!("observatory_site:{site_id}"))
                    || snap
                        .service_state_blobs
                        .contains_key(&format!("observatory_completed:{site_id}"))
            });
            if !obs_exists_or_done {
                let _ = spawn_observatory_flashpoint(ctx, sys_id).await;
            }
        }
    }
}
