//! Remediation tests for exploration, DED complexes, and J-space Sleeper combat.
//
// Parity Tags Tested:
// - feat:pve.combat-signatures
// - feat:pve.data-sites
// - feat:pve.relic-sites
// - feat:pve.loot-spew
// - feat:pve.ded-complexes
// - feat:pve.wormhole-sites

#![forbid(unsafe_code)]

use std::collections::HashMap;
use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_mut_combat_runtime;
use eve_server::services::pve_exploration::{ensure_system_pve_sites_seeded, with_pve_exploration_reg};
use eve_server::services::pve_exploration_sites::{generate_exploration_container_loot, overseer_effects_for_rating};
use eve_machonet::SessionState;

const AU: f64 = 149_597_870_700.0;
const CHAR_ID: i64 = 90_000_001;

async fn place_ship(ctx: &ServerContext, sys: u32, ship_id: i64, type_id: u32, pos: DVec3) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id, owner_id: CHAR_ID, location_id: sys as i64,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default()
        });
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.active_ship_id = ship_id;
        }
    });
    let _ = ctx.systems.get_or_spawn(sys).spawn_ball(Ball {
        id: ship_id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 365.0, inertia: 1.0,
        mass: 1_067_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
}

async fn create_in_space_session(ctx: &ServerContext, session_id: u64, sys: u32, ship_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(CHAR_ID)], &kw, None).await.expect("SelectCharacterID");
    sess.solar_system_id = Some(sys);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys;
        }
    });
    place_ship(ctx, sys, ship_id, 587, DVec3::ZERO).await;
    sess
}

/// Destroys every active NPC of a site through the production in-space damage pipeline
/// (`SystemHandle::apply_damage_with_attacker` -> `npc_death_hook`).
async fn kill_ball(ctx: &ServerContext, sys: u32, bid: i64) {
    let dmg = DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 };
    let res = ctx.systems.get_or_spawn(sys).apply_damage_with_attacker(bid, dmg, Some(CHAR_ID)).await;
    assert!(matches!(res, Ok(Some((_, true, _)))), "NPC ball {bid} must be destroyed by damage: {res:?}");
}

fn site_state(ctx: &ServerContext, sys: u32, site_id: &str) -> (u8, Vec<i64>, bool, [f64; 3]) {
    with_pve_exploration_reg(ctx, |reg| {
        let s = reg.sites_by_system[&sys].iter().find(|s| s.site_id == site_id).expect("site");
        (s.current_wave, s.active_npc_ball_ids.clone(), s.cleared, s.position)
    })
}

async fn kill_site_wave(ctx: &ServerContext, sys: u32, site_id: &str) {
    for bid in site_state(ctx, sys, site_id).1 {
        kill_ball(ctx, sys, bid).await;
    }
}

async fn activate_gate(ctx: &ServerContext, sess: &mut SessionState, site: &str) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::String(site.into())], &HashMap::new(), None).await
}

fn container_cargo(ctx: &ServerContext, container_id: i64) -> Vec<InventoryItemRecord> {
    ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.location_id == container_id && i.flag == InventoryFlag::Cargo).cloned().collect()
    })
}

fn probes_dict(ids: &[i64], center: DVec3, range: f64) -> PyRep {
    let d = range * 0.4;
    let offs = [DVec3::ZERO, DVec3::X * d, -DVec3::X * d, DVec3::Y * d, -DVec3::Y * d, DVec3::Z * d, -DVec3::Z * d, DVec3::splat(d * 0.5)];
    PyRep::Dict(ids.iter().enumerate().map(|(i, &pid)| {
        let p = center + offs[i % offs.len()];
        (PyRep::Integer(pid), PyRep::keyval([
            ("pos", PyRep::Tuple(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)])),
            ("scanRange", PyRep::Real(range)),
        ]))
    }).collect())
}

fn row_field(rows: &[PyRep], sig_id: &str, key: &str) -> PyRep {
    rows.iter().find(|r| r.dict_get("signatureID").and_then(|v| v.as_str()) == Some(sig_id))
        .and_then(|r| r.dict_get(key).cloned())
        .unwrap_or_else(|| panic!("{sig_id} row missing {key}"))
}

async fn hack_until_won(ctx: &ServerContext, sess: &mut SessionState, can_id: i64) -> PyRep {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_id)], &kw, None).await.expect("StartNewGameInstance");
    let core_idx = with_pve_exploration_reg(ctx, |reg| {
        reg.hacking_games[&(CHAR_ID, can_id)].tiles.iter().find(|t| format!("{:?}", t.kind) == "SystemCore").unwrap().tile_index
    });
    for _ in 0..6 {
        let r = ctx.dispatch_rpc(sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(core_idx as i64)], &kw, None).await.expect("ClickedOnTile");
        if r.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
            return r;
        }
    }
    panic!("hack on {can_id} was not won");
}

/// Asserts won-hack loot exists only inside the spewed in-space cans (once each) and never in the parent can.
fn assert_spew_loot(ctx: &ServerContext, won: &PyRep, parent_can: i64, sys: u32) -> Vec<u32> {
    assert!(container_cargo(ctx, parent_can).is_empty(), "Parent container must not duplicate spewed loot");
    let Some(PyRep::List(spew)) = won.dict_get("spewedContainers") else { panic!("spewedContainers") };
    assert!(!spew.is_empty(), "Won hack must spew loot cans");
    let mut types = Vec::new();
    for c in spew {
        let can_id = c.dict_get("canItemID").and_then(|v| v.as_i64()).unwrap();
        let can = ctx.db.with_snapshot(|s| s.items.get(&can_id).cloned()).expect("spewed can is a real item");
        assert_eq!(can.location_id, sys as i64, "Spewed can must sit in space");
        let cargo = container_cargo(ctx, can_id);
        assert_eq!(cargo.len(), 1, "Each spewed can holds exactly one loot stack");
        assert_eq!(cargo[0].type_id as i64, c.dict_get("lootTypeID").and_then(|v| v.as_i64()).unwrap());
        types.push(cargo[0].type_id);
    }
    types
}

// ============================================================================
// 1. Probe-scanned Combat Signatures, SDE Data/Relic Loot & Loot Spew
// ============================================================================
#[tokio::test]
async fn test_pve_data_relic_loot_tables_container_inventory_and_combat_signatures() {
    // parity: feat:pve.combat-signatures, feat:pve.data-sites, feat:pve.relic-sites, feat:pve.loot-spew
    eve_testkit::attest(&["feat:pve.combat-signatures", "feat:pve.data-sites", "feat:pve.relic-sites", "feat:pve.loot-spew"]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let sys = 30_000_142_u32;
    let ship_id = 710_001_101_i64;
    let mut sess = create_in_space_session(&ctx, 8201, sys, ship_id).await;
    ensure_system_pve_sites_seeded(&ctx, sys).await;

    // 1. Launch 8 Core Scanner Probes from a fitted Core Probe Launcher I (`dogmaIM.Activate`)
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(CHAR_ID).or_default();
        for sid in [3402u32, 3412, 3413, 3426] {
            skills.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        for (iid, tid, qty) in [(440_101_i64, 17938_u32, 1), (440_102, 30013, 8)] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: CHAR_ID, location_id: ship_id, flag: InventoryFlag::HiSlot0,
                quantity: qty, singleton: qty == 1, ..Default::default()
            });
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let launch = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(440_101), PyRep::String("launchScanners".into()), PyRep::None, PyRep::Integer(0)], &kw, None).await.expect("launch probes");
    let Some(PyRep::List(pids)) = launch.dict_get("probeIDs") else { panic!("probeIDs") };
    let probe_ids: Vec<i64> = pids.iter().filter_map(|p| p.as_i64()).collect();
    assert_eq!(probe_ids.len(), 8);
    let bound = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr");
    let bound_id = bound.dict_get("boundID").and_then(|v| v.as_i64()).unwrap() as u64;

    // 2. scanMgr.RequestScans: probes 40 AU away from DAT-<sys> register zero signal for it
    let (dat_name, dat_pos) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&sys].iter().find(|s| s.site_id == "DAT-30000142").unwrap();
        (s.dungeon_name.clone(), DVec3::from_array(s.position))
    });
    let far = probes_dict(&probe_ids, dat_pos + DVec3::X * 40.0 * AU, 0.25 * AU);
    let far_rep = ctx.dispatch_rpc(&mut sess, "scanMgr", Some(bound_id), "RequestScans", &[far], &kw, None).await.expect("far RequestScans");
    let PyRep::List(far_rows) = far_rep else { panic!("RequestScans rows") };
    assert_eq!(row_field(&far_rows, "DAT-30000142", "signalStrength").as_f64(), Some(0.0), "Out-of-range probes give no signal");
    assert_eq!(row_field(&far_rows, "DAT-30000142", "coveringProbes").as_i64(), Some(0));

    // 3. Pinpoint 8-probe formation centred on the signature resolves it to 100% and reveals its SDE dungeon name
    let mut rows = Vec::new();
    for _ in 0..4 {
        let rep = ctx.dispatch_rpc(&mut sess, "scanMgr", Some(bound_id), "RequestScans", &[probes_dict(&probe_ids, dat_pos, 0.25 * AU)], &kw, None).await.expect("pinpoint RequestScans");
        let PyRep::List(r) = rep else { panic!() };
        rows = r;
    }
    assert_eq!(row_field(&rows, "DAT-30000142", "coveringProbes").as_i64(), Some(8));
    assert_eq!(row_field(&rows, "DAT-30000142", "signalStrength").as_f64(), Some(1.0));
    assert_eq!(row_field(&rows, "DAT-30000142", "type").as_str(), Some(dat_name.as_str()));

    // 4. Data can hack: loot spews into real in-space cans with SDE Datacores (333) and Decryptors (728-735, 1304)
    let data_can_id = 1_870_000_000_i64 + (sys as i64 % 100_000) * 100;
    let won = hack_until_won(&ctx, &mut sess, data_can_id).await;
    let data_types = assert_spew_loot(&ctx, &won, data_can_id, sys);
    let grp = |t: &u32| ctx.sde.types.get(t).map(|x| x.group_id).unwrap_or(0);
    assert!(data_types.iter().any(|t| grp(t) == 333), "Data spew must contain SDE Datacores");
    assert!(data_types.iter().any(|t| matches!(grp(t), 728..=735 | 1304)), "Data spew must contain SDE Decryptors");
    let expected: Vec<u32> = generate_exploration_container_loot(&ctx.sde, sys, false).iter().map(|l| l.0).collect();
    assert_eq!(data_types, expected, "Spewed loot is exactly the SDE-derived loot table");
    let hack_again = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "HackContainer", &[PyRep::Integer(data_can_id)], &kw, None).await;
    assert!(hack_again.is_err(), "Non-catalog hackingMgr.HackContainer must not be served");

    // 5. Relic can hack: SDE Salvaged Materials (754); Ancient Salvage (966) only in null-sec / J-space
    let relic_can_id = 1_871_000_000_i64 + (sys as i64 % 100_000) * 100;
    let won = hack_until_won(&ctx, &mut sess, relic_can_id).await;
    let relic_types = assert_spew_loot(&ctx, &won, relic_can_id, sys);
    assert!(relic_types.iter().all(|t| grp(t) == 754), "High-sec relic spew is SDE Salvaged Materials only");
    let jspace = generate_exploration_container_loot(&ctx.sde, 31_000_003, true);
    assert!(jspace.iter().any(|l| grp(&l.0) == 966), "J-space relic loot includes SDE Ancient Salvage");
    let hs_qty: i64 = generate_exploration_container_loot(&ctx.sde, sys, true).iter().map(|l| l.1).sum();
    let js_qty: i64 = jspace.iter().filter(|l| grp(&l.0) == 754).map(|l| l.1).sum();
    assert!(js_qty > hs_qty, "True-sec scaling: J-space salvage {js_qty} must exceed high-sec {hs_qty}");

    // 6. Faction weighting: Guristas (The Forge) and Serpentis regions pick different SDE datacores first
    let guristas = generate_exploration_container_loot(&ctx.sde, sys, false);
    let serpentis = generate_exploration_container_loot(&ctx.sde, 30_002_659, false);
    assert_ne!(guristas[0].0, serpentis[0].0, "Pirate faction must change the weighted datacore selection");
}

// ============================================================================
// 2. DED Gates, Overseer Wreck on Overseer Kill; Wormhole Sleeper Waves via NPC Kills
// ============================================================================
#[tokio::test]
async fn test_pve_ded_complexes_gating_overseer_wrecks_and_wormhole_sleepers() {
    // parity: feat:pve.ded-complexes, feat:pve.wormhole-sites
    eve_testkit::attest(&["feat:pve.ded-complexes", "feat:pve.wormhole-sites"]);

    let ctx = ServerContext::new();
    let sys = 30_000_142_u32;
    let ship_id = 710_001_102_i64;
    let mut sess = create_in_space_session(&ctx, 8202, sys, ship_id).await;
    ensure_system_pve_sites_seeded(&ctx, sys).await;
    let ded = "DED-30000142-3";

    // 1. Killing room 1 NPCs through the damage pipeline unlocks (but does not auto-advance) the gate
    kill_site_wave(&ctx, sys, ded).await;
    let (wave, active, _, ded_pos) = site_state(&ctx, sys, ded);
    assert_eq!((wave, active.len()), (1, 0), "DED rooms advance only through the acceleration gate");

    // 2. Gate proximity: 0 m origin is far from the gate; within 2,500 m the Rifter passes to room 2
    let r = activate_gate(&ctx, &mut sess, ded).await;
    assert_eq!(r.expect_err("far from gate").code, "GateOutOfRange");
    place_ship(&ctx, sys, ship_id, 587, DVec3::from_array(ded_pos) + DVec3::X * 1_000.0).await;
    let r = activate_gate(&ctx, &mut sess, ded).await;
    assert_eq!(r.expect("Rifter at gate").dict_get("currentWave").and_then(|v| v.as_i64()), Some(2));

    // 3. Battleship (Raven 638) rejected by the canonical DED 3/10 hull limit (Frigates & Destroyers only)
    let raven_id = 710_001_103_i64;
    place_ship(&ctx, sys, raven_id, 638, DVec3::from_array(ded_pos)).await;
    sess.ship_id = Some(raven_id);
    let r = activate_gate(&ctx, &mut sess, ded).await;
    assert_eq!(r.expect_err("Raven in 3/10").code, "ShipSizeRestricted");
    let kw = HashMap::new();
    let r = ctx.dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::String(ded.into()), PyRep::Integer(10)], &kw, None).await;
    assert_eq!(r.expect_err("client rating override ignored").code, "ShipSizeRestricted", "Server-side DED rating cannot be overridden");
    let ded_dungeon_alias = with_pve_exploration_reg(&ctx, |reg| reg.sites_by_system[&sys].iter().find(|s| s.site_id == ded).map(|s| s.dungeon_id.to_string())).unwrap();
    let r = activate_gate(&ctx, &mut sess, &ded_dungeon_alias).await;
    assert_eq!(r.expect_err("dungeon-id alias").code, "ShipSizeRestricted", "str(dungeon_id) alias resolves to the DED site and is hull-checked");
    let cruiser = eve_server::services::pve_exploration_sites::check_ded_gate_ship_allowed(&ctx.sde, 3, 620, 10_000_000.0);
    assert!(cruiser.is_err(), "Cruisers (Osprey 620) are not allowed in DED 3/10");
    place_ship(&ctx, sys, ship_id, 587, DVec3::ZERO).await;
    sess.ship_id = Some(ship_id);
    let r = activate_gate(&ctx, &mut sess, &ded_dungeon_alias).await;
    assert_eq!(r.expect_err("alias out of range").code, "GateOutOfRange", "str(dungeon_id) alias is proximity-checked");
    place_ship(&ctx, sys, ship_id, 587, DVec3::from_array(ded_pos) + DVec3::X * 1_000.0).await;

    // 4. Room 2 kills -> gate -> room 3; killing the final-room overseer spawns the Overseer wreck in space
    kill_site_wave(&ctx, sys, ded).await;
    // Integer argument shape: keeper.ActivateAccelerationGate(gate_item_id) on the seeded DED gate ball
    let gate_ball_id = with_pve_exploration_reg(&ctx, |reg| reg.sites_by_system[&sys].iter().find(|s| s.site_id == ded).and_then(|s| s.gate_ball_id)).expect("DED gate ball");
    assert!(ctx.systems.get_or_spawn(sys).get_ball_snapshot(gate_ball_id).await.unwrap().is_some(), "DED gate ball in space");
    let r = ctx.dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(gate_ball_id)], &kw, None).await;
    assert_eq!(r.expect("gate to room 3").dict_get("currentWave").and_then(|v| v.as_i64()), Some(3));
    kill_site_wave(&ctx, sys, ded).await;
    assert!(site_state(&ctx, sys, ded).2, "Overseer kill completes the DED complex");
    let (ope_tid, ope_name) = overseer_effects_for_rating(&ctx.sde, 3).expect("SDE Overseer's Personal Effects");
    assert!((19_400..=19_422).contains(&ope_tid) && ope_name.contains("Overseer"));
    let wreck = ctx.db.with_snapshot(|snap| {
        snap.items.values().find(|w| w.location_id == sys as i64 && w.type_id == 26473
            && snap.items.values().any(|i| i.location_id == w.item_id && i.type_id == ope_tid)).map(|w| w.item_id)
    }).expect("Overseer wreck in space holds the SDE Overseer's Personal Effects");
    assert!(container_cargo(&ctx, wreck).iter().any(|i| i.type_id == 18658), "Wreck holds the deadspace module");
    assert!(ctx.systems.get_or_spawn(sys).get_ball_snapshot(wreck).await.unwrap().is_some(), "Wreck ball spawned");
    assert!(container_cargo(&ctx, ship_id).iter().all(|i| i.type_id != ope_tid), "No loot teleported to ship");
    let r = activate_gate(&ctx, &mut sess, ded).await;
    assert_eq!(r.expect_err("cleared").code, "SiteAlreadyCleared");

    // 5. C3 Sleeper site seeded by production `ensure_system_pve_sites_seeded` with zero-bounty Sleepers
    let c3 = 31_000_003_u32;
    ensure_system_pve_sites_seeded(&ctx, c3).await;
    let wh_site = format!("PVE-ANOM-{c3}-1");
    let (w1, wave1, _, _) = site_state(&ctx, c3, &wh_site);
    assert_eq!(w1, 1);
    let (trigger_type, bounty) = with_mut_combat_runtime(&ctx, |rt| {
        let n = &rt.npc_entities[&wave1[0]];
        (n.type_id, n.bounty_isk)
    });
    assert_eq!((trigger_type, bounty), (30208, 0.0), "Wave trigger is the Awakened Warden with zero bounty");

    // 6. Non-trigger kill drops a Sleeper wreck with SDE Blue Loot (group 880) but does not advance the wave
    kill_ball(&ctx, c3, wave1[1]).await;
    assert_eq!(site_state(&ctx, c3, &wh_site).0, 1);
    let blue = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|w| w.location_id == c3 as i64 && w.type_id == 30495)
            .any(|w| snap.items.values().any(|i| i.location_id == w.item_id && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 880)))
    });
    assert!(blue, "Sleeper wreck must contain SDE Sleeper Components (Blue Loot)");

    // 7. Trigger kill spawns wave 2 automatically; leftovers stay; then clear every wave to finish the site
    kill_ball(&ctx, c3, wave1[0]).await;
    let (w2, active2, _, _) = site_state(&ctx, c3, &wh_site);
    assert_eq!(w2, 2, "Trigger NPC kill advances to wave 2 without any client RPC");
    assert!(active2.contains(&wave1[2]), "Surviving wave-1 Sleepers remain in the site");
    for _ in 0..20 {
        let (_, active, cleared, _) = site_state(&ctx, c3, &wh_site);
        if cleared {
            break;
        }
        kill_ball(&ctx, c3, active[0]).await;
    }
    assert!(site_state(&ctx, c3, &wh_site).2, "All Sleeper waves destroyed -> site cleared");
    let pending = with_mut_combat_runtime(&ctx, |rt| rt.pending_bounty_ticks.get(&(c3, CHAR_ID)).copied().unwrap_or(0.0));
    assert_eq!(pending, 0.0, "CONCORD pays no bounty for Sleeper kills");
}
