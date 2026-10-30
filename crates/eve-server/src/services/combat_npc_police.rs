//! `combat_npc_police` — High-security Faction Police responses to outlaws and criminal pilots (`feat:npc.navy-npc`, `feat:crimewatch.faction-police`).
//!
//! Below security threshold (<= -5.0) in high-sec (system security >= 0.45):
//! - Faction Police NPC ships spawn on grid targeting the outlaw ship.
//! - Docking is refused to outlaw ships.
//! - Pods/capsules at <= -5.0 are NEVER engaged by Faction Police, and capsules are allowed to dock.
//! - Pilots with security status > -5.0 (and standing > -5.0) are NOT engaged.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity;
use crate::services::combat_crimewatch::crimewatch_timers::faction_police_sec_threshold;
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use eve_destiny::DamageVector;
use eve_sde::SolarSystemId;
use glam::DVec3;

/// Resolves the authentic Faction Police ship type ID for a given empire faction.
pub fn police_ship_type_for_faction(faction_id: u32) -> u32 {
    match faction_id {
        500_001 => 9970,  // Caldari Police 3rd Lieutenant
        500_004 => 9984,  // Gallente Police Captain
        500_003 => 3768,  // Amarr Police Frigate
        500_002 => 10046, // Minmatar Police / Navy Captain
        _ => 9970,
    }
}

/// Evaluates whether an outlaw ship in highsec is refused docking permissions (`feat:npc.navy-npc`).
pub fn is_outlaw_ship_docking_refused(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    char_id: i64,
    ship_id: i64,
) -> bool {
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sys_sec < 0.45
        || is_concord_suppressed_in_system(ctx, solar_system_id)
        || is_sentry_disabled_by_corruption(ctx, solar_system_id)
    {
        return false;
    }
    let (char_sec, is_capsule) = ctx.db.with_snapshot(|snap| {
        let sec = snap.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0);
        let is_pod = snap.items.get(&ship_id).map(|i| {
            i.type_id == 670 || ctx.sde.types.get(&i.type_id).map(|t| t.group_id == 29).unwrap_or(false)
        }).unwrap_or(false);
        (sec, is_pod)
    });
    if is_capsule {
        return false; // Capsules are allowed to dock
    }
    let threshold = faction_police_sec_threshold(sys_sec).unwrap_or(-5.0);
    char_sec <= threshold || char_sec <= -5.0
}

/// Checks the ballpark for outlaw pilots (sec status <= -5.0) in high-sec ships and spawns Faction Police (`feat:npc.navy-npc`).
pub async fn check_and_spawn_faction_police(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
) -> Vec<i64> {
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sys_sec < 0.45
        || is_concord_suppressed_in_system(ctx, solar_system_id)
        || is_faction_police_suppressed(ctx, solar_system_id)
    {
        return Vec::new();
    }
    let threshold = faction_police_sec_threshold(sys_sec).unwrap_or(-5.0);
    let sys_faction = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .and_then(|sys| {
            ctx.sde
                .constellations
                .get(&sys.constellation_id)
                .and_then(|c| c.faction_id)
                .or_else(|| ctx.sde.regions.get(&sys.region_id).and_then(|r| r.faction_id))
        })
        .unwrap_or(500_001);
    let police_type = police_ship_type_for_faction(sys_faction);

    let handle = ctx.systems.get_or_spawn(solar_system_id);
    let ball_ids = handle
        .list_balls()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|b| b.id)
        .collect::<Vec<_>>();

    let mut spawned_police = Vec::new();

    for bid in ball_ids {
        let snap_opt = handle.get_ball_snapshot(bid).await.ok().flatten();
        let Some((_, ball)) = snap_opt else { continue; };

        // Exclude capsules (type 670 / group 29) and non-ships
        if ball.type_id == 670 || ctx.sde.types.get(&ball.type_id).map(|t| t.group_id == 29).unwrap_or(false) {
            continue;
        }

        // Find active piloted character owner in this solar system (R-CUSTOMS-SCOPE)
        let owner_char_opt = ctx.db.with_snapshot(|snap| {
            snap.characters
                .values()
                .find(|c| c.active_ship_id == bid && c.solar_system_id == solar_system_id && c.station_id.is_none())
                .map(|c| (c.character_id, c.security_status))
        });
        let Some((_char_id, sec_status)) = owner_char_opt else { continue; };

        // High-sec customs contraband scan & enforcement (`feat:ships.blockade-runner-cargo-scan-immunity`, `feat:attributes.booster-highsec-contraband`):
        // Blockade Runners (group 1202, cargoScanResistance == 100%) are immune to player cargo scanners,
        // but high-sec NPC customs enforce contraband rules (confiscation, fine, standing loss) on all hulls.
        let detected = scan_and_enforce_highsec_contraband(ctx, solar_system_id, bid);
        let has_contraband = !detected.is_empty();

        // Only engage outlaws below threshold or <= -5.0, or ships carrying contraband
        if !has_contraband && sec_status > threshold && sec_status > -5.0 {
            continue;
        }

        // Check if Faction Police already targets this ship on grid
        let already_targeted = with_mut_combat_runtime(ctx, |rt| {
            rt.npc_entities.values().any(|n| {
                n.archetype == "FactionPolice"
                    && n.solar_system_id == solar_system_id
                    && (n.target_id == Some(bid) || n.aggression_threat.contains_key(&bid))
            })
        });
        if already_targeted {
            continue;
        }

        // Break gate cloak on outlaw
        let _ = handle.break_gate_cloak(bid).await;

        // Spawn Faction Police NPC ship on grid 3,000m from target
        let police_pos = ball.position + DVec3::new(3_000.0, 0.0, 0.0);
        let police_ball_id = spawn_specialized_npc_entity(
            ctx,
            solar_system_id,
            police_type,
            "FactionPolice",
            sys_faction,
            police_pos,
        )
        .await;

        // Configure police combat properties and lock target
        with_mut_combat_runtime(ctx, |rt| {
            if let Some(npc) = rt.npc_entities.get_mut(&police_ball_id) {
                npc.faction_id = sys_faction;
                npc.target_id = Some(bid);
                npc.aggression_threat.insert(bid, 500.0);
                npc.threat_table.insert(bid, 500.0);
                npc.optimal_range = 25_000.0;
                npc.orbit_range = 5_000.0;
                npc.damage_vector = DamageVector {
                    em: 50.0,
                    thermal: 50.0,
                    kinetic: 50.0,
                    explosive: 50.0,
                };
                npc.web_speed_factor = -50.0;
                npc.web_range = 15_000.0;
                npc.scram_strength = 2.0;
                npc.scram_range = 15_000.0;
            }
        });

        // Add ballpark lock
        let _ = handle
            .add_target_lock(
                police_ball_id,
                eve_destiny::TargetLock {
                    target_id: bid,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(50_000.0),
                },
            )
            .await;

        spawned_police.push(police_ball_id);
    }

    spawned_police
}

/// NPC customs contraband scan and enforcement in high-security space
/// (`feat:ships.blockade-runner-cargo-scan-immunity`, `feat:attributes.booster-highsec-contraband`).
///
/// Note: Blockade Runners (`group 1202`, `cargoScanResistance == 100%`) are immune to player Cargo Scanners (`group 47`),
/// but NPC customs authorities in high-sec still scan and detect contraband cargo inside Blockade Runners.
/// Enforces confiscation of contraband items/boosters in cargo, wallet fines, and faction standing loss.
// parity: feat:ships.blockade-runner-cargo-scan-immunity
pub fn scan_and_enforce_highsec_contraband(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    ship_id: i64,
) -> Vec<(u32, i32, f64)> {
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sys_sec < 0.45
        || is_concord_suppressed_in_system(ctx, solar_system_id)
        || is_faction_police_suppressed(ctx, solar_system_id)
    {
        return Vec::new();
    }
    let sys_faction = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .and_then(|sys| {
            ctx.sde
                .constellations
                .get(&sys.constellation_id)
                .and_then(|c| c.faction_id)
                .or_else(|| ctx.sde.regions.get(&sys.region_id).and_then(|r| r.faction_id))
        })
        .unwrap_or(500_001);
    let char_id = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .find(|c| c.active_ship_id == ship_id && c.solar_system_id == solar_system_id && c.station_id.is_none())
            .map(|c| c.character_id)
    });
    let Some(char_id) = char_id else {
        return Vec::new();
    };
    let contraband_map = ctx.sde.load_contraband_types().unwrap_or_default();
    let now_unix = ctx.now_unix();

    let (confiscated, found) = ctx.db.with_mut_snapshot(|snap| {
        let contraband_items: Vec<(i64, u32, i32, f64, f64)> = snap
            .items
            .values()
            .filter_map(|it| {
                if it.location_id != ship_id || it.flag != eve_db::InventoryFlag::Cargo || it.quantity <= 0 {
                    return None;
                }
                let t = ctx.sde.types.get(&it.type_id)?;
                // Check SDE contraband types table:
                if let Some(entries) = contraband_map.get(&it.type_id) {
                    if let Some(entry) = entries.get(&sys_faction).filter(|e| {
                        sys_sec >= e.confiscate_min_sec as f64
                    }) {
                        let unit_price = t.base_price.max(100.0);
                        let fine = unit_price * (it.quantity as f64) * (entry.fine_by_value as f64).max(1.0);
                        let standing_loss = (entry.standing_loss as f64).min(-0.1);
                        return Some((it.item_id, it.type_id, it.quantity, fine, standing_loss));
                    }
                }
                // Check contraband combat boosters (group 303 with side-effect chance attr 1089 > 0.0):
                let is_contraband_booster = t.group_id == 303
                    && t.default_attributes.get(&1089).copied().unwrap_or(0.0) > 0.0;
                if is_contraband_booster {
                    let fine = (t.base_price.max(10_000.0)) * (it.quantity as f64);
                    return Some((it.item_id, it.type_id, it.quantity, fine, -0.1));
                }
                None
            })
            .collect();

        if contraband_items.is_empty() {
            return (Vec::new(), Vec::new());
        }

        let mut removed = Vec::new();
        let mut found_list = Vec::new();
        let mut total_fine_cents: i64 = 0;
        let mut min_standing_loss: f64 = -0.1;

        for (iid, tid, qty, fine_isk, st_loss) in contraband_items {
            found_list.push((tid, qty, fine_isk));
            min_standing_loss = min_standing_loss.min(st_loss);
            if let Some(mut it) = snap.items.remove(&iid) {
                let old_loc = it.location_id;
                let old_flag = it.flag;
                it.location_id = 6; // confiscated / junkyard
                it.flag = eve_db::InventoryFlag::None;
                total_fine_cents = total_fine_cents.saturating_add((fine_isk * 100.0).round() as i64);
                removed.push((it, old_loc, old_flag, fine_isk));
            }
        }

        if char_id > 0 {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                let deduct = total_fine_cents.min(ch.balance.0.max(0));
                if deduct > 0 {
                    ch.balance = eve_db::IskCents(ch.balance.0 - deduct);
                    let new_bal = ch.balance;
                    snap.next_entity_id += 1;
                    let jid = snap.next_entity_id;
                    snap.wallet_journal.push(eve_db::WalletJournalEntry {
                        entry_id: jid,
                        character_id: char_id,
                        entry_type: "contraband_fine".into(),
                        amount: eve_db::IskCents(-deduct),
                        balance_after: new_bal,
                        reason: format!("High-sec customs contraband confiscation fine (faction {sys_faction})"),
                    });
                    snap.wallet_journal_timestamps.insert(jid, now_unix);
                }
            }

            snap.next_entity_id += 1;
            let st_event_id = snap.next_entity_id;
            snap.standing_transactions
                .entry(char_id)
                .or_default()
                .push(eve_db::StandingTransactionRecord {
                    event_id: st_event_id,
                    from_id: sys_faction as i64,
                    to_id: char_id,
                    modification: min_standing_loss,
                    event_type_id: 85, // contraband confiscation standing penalty
                });
        }

        (removed, found_list)
    });

    if char_id > 0 {
        for (removed_item, old_loc, old_flag, fine_isk) in confiscated {
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnItemChange",
                "charid",
                vec![
                    eve_proto::PyRep::keyval([
                        ("itemID", eve_proto::PyRep::Integer(removed_item.item_id)),
                        ("typeID", eve_proto::PyRep::Integer(removed_item.type_id as i64)),
                        ("ownerID", eve_proto::PyRep::Integer(removed_item.owner_id)),
                        ("locationID", eve_proto::PyRep::Integer(removed_item.location_id)),
                        ("flagID", eve_proto::PyRep::Integer(removed_item.flag as i64)),
                        ("quantity", eve_proto::PyRep::Integer(removed_item.quantity as i64)),
                    ]),
                    eve_proto::PyRep::Dict(vec![
                        (eve_proto::PyRep::Integer(3), eve_proto::PyRep::Integer(old_loc)),
                        (eve_proto::PyRep::Integer(4), eve_proto::PyRep::Integer(old_flag as i64)),
                    ]),
                ],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnRemoteMessage",
                "charid",
                vec![
                    eve_proto::PyRep::String("ParamCustomsNotification".into()),
                    eve_proto::PyRep::Dict(vec![
                        (eve_proto::PyRep::String("typeID".into()), eve_proto::PyRep::Integer(removed_item.type_id as i64)),
                        (eve_proto::PyRep::String("quantity".into()), eve_proto::PyRep::Integer(removed_item.quantity as i64)),
                        (eve_proto::PyRep::String("fineISK".into()), eve_proto::PyRep::Real(fine_isk)),
                        (eve_proto::PyRep::String("factionID".into()), eve_proto::PyRep::Integer(sys_faction as i64)),
                    ]),
                ],
            );
        }
    }

    found
}

/// Returns whether CONCORD response is suppressed in the solar system (e.g., Insurgency Corruption Stage 5).
pub fn is_concord_suppressed_in_system(ctx: &ServerContext, solar_system_id: SolarSystemId) -> bool {
    crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, solar_system_id) >= 5
}

/// Returns whether gate and station sentry guns are disabled in the solar system
/// (Insurgency Corruption Stage 5 in low-sec, unless Counter-Insurgency Sentry Guns are active).
pub fn is_sentry_disabled_by_corruption(ctx: &ServerContext, solar_system_id: SolarSystemId) -> bool {
    let sys_sec = ctx.sde.solar_systems.get(&solar_system_id).map(|s| s.security_status).unwrap_or(0.0);
    0.0 < sys_sec
        && sys_sec < 0.45
        && crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, solar_system_id) >= 5
        && !crate::services::security_mgr::has_active_counter_insurgency_sentries(ctx, solar_system_id)
}

/// Returns whether Empire Navy / Faction Police patrols are suppressed (Insurgency Corruption Stage 2+).
pub fn is_faction_police_suppressed(ctx: &ServerContext, solar_system_id: SolarSystemId) -> bool {
    crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, solar_system_id) >= 2
}
