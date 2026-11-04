//! End-to-end V24.01 integration tests closing NPC spawns, belt rats, Clone Soldiers,
//! Mordu's Legion Special Warfare Units, Faction/Null-Sec Officers, and Drifters.
//!
//! SDE Data Gap Note (intentionally left open without parity tags):
//! `data/evomu_sde.sqlite` `sde_dungeons` stores only `(dungeon_id, name, archetype_id, faction_id, difficulty)`
//! without multi-room acceleration gate graphs or per-wave trigger scripts for World Ark Observatories,
//! Pirate Forward Operating Bases, or Insurgency Ice Heist / Mining Ambush / Outpost Raid / Laundering sites.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    combat_npc_encounters::flush_npc_bounty_tick, with_combat_runtime,
};
use eve_server::session_cluster_lifecycle::tick_background_cluster;
use std::collections::HashMap;

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

pub(crate) async fn bind_beyonce(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32) -> u64 {
    let PyRep::Tuple(tup) = rpc(
        ctx,
        sess,
        "beyonce",
        "MachoBindObject",
        &[PyRep::Integer(sys_id as i64), PyRep::None],
    )
    .await
    .expect("MachoBindObject beyonce") else {
        panic!("expected Tuple from MachoBindObject");
    };
    tup[0].as_i64().expect("bound_id") as u64
}

pub(crate) fn find_multi_hit_seed(ctx: &ServerContext, prob: f64, count: usize) -> u64 {
    (1u64..500_000)
        .find(|&s| {
            ctx.set_mechanic_rng_seed(Some(s));
            (0..count).all(|_| ctx.mechanic_rng.roll(prob))
        })
        .expect("deterministic seed hitting consecutive rolls")
}

fn grant_combat_skills(ctx: &ServerContext, char_id: i64, type_ids: &[u32]) {
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for &sid in &[
            3300u32, 3308, 3310, 3311, 3312, 3315, 3316, 3318, 3413, 3418, 3426, 20327, 21667,
            41404, 54794, 54826, 54829, 55033,
        ] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 1_280_000.0 });
        }
        for &tid in type_ids {
            if let Some(t) = ctx.sde.types.get(&tid) {
                for sa in [
                    eve_sde::attr_ids::REQUIRED_SKILL_1,
                    eve_sde::attr_ids::REQUIRED_SKILL_2,
                    eve_sde::attr_ids::REQUIRED_SKILL_3,
                ] {
                    if let Some(&req_f) = t.default_attributes.get(&sa) {
                        let rsid = req_f as u32;
                        if rsid > 0 {
                            sk.insert(rsid, TrainedSkillRecord { type_id: rsid, level: 5, skill_points: 1_280_000.0 });
                        }
                    }
                }
            }
        }
    });
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, qty: i32) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity: qty,
        singleton: qty == 1,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

pub(crate) fn fit_combat_ship(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_tid: u32,
    loc_id: i64,
    turret_tid: u32,
    ammo_tid: u32,
    base_mod_id: i64,
) -> [i64; 3] {
    let dmg_mod_tid = if turret_tid == 54754 { 54975u32 } else { 519u32 };
    grant_combat_skills(ctx, char_id, &[ship_tid, turret_tid, ammo_tid, dmg_mod_tid, 3504]);
    let guns = [base_mod_id, base_mod_id + 1, base_mod_id + 2];
    let hi_flags = [InventoryFlag::HiSlot0, InventoryFlag::HiSlot1, InventoryFlag::HiSlot2];
    let med_flags = [InventoryFlag::MedSlot0, InventoryFlag::MedSlot1, InventoryFlag::MedSlot2];
    let lo_flags = [InventoryFlag::LoSlot0, InventoryFlag::LoSlot1, InventoryFlag::LoSlot2];
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.insert(ship_id, make_item(ship_id, ship_tid, char_id, loc_id, InventoryFlag::Hangar, 1));
        for i in 0..3 {
            snap.items.insert(guns[i], make_item(guns[i], turret_tid, char_id, ship_id, hi_flags[i], 1));
            snap.items.insert(base_mod_id + 10 + i as i64, make_item(base_mod_id + 10 + i as i64, ammo_tid, char_id, ship_id, hi_flags[i], 5_000));
            let dmg_id = base_mod_id + 20 + i as i64;
            snap.items.insert(dmg_id, make_item(dmg_id, dmg_mod_tid, char_id, ship_id, lo_flags[i], 1));
            snap.module_online_states.insert(guns[i], true);
            snap.module_online_states.insert(dmg_id, true);
            if ship_tid == 54733 {
                let cap_id = base_mod_id + 30 + i as i64;
                snap.items.insert(cap_id, make_item(cap_id, 3504, char_id, ship_id, med_flags[i], 1));
                snap.module_online_states.insert(cap_id, true);
            }
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    guns
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn setup_docked_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    secret: &str,
    char_id: i64,
    sys_id: u32,
    station_id: i64,
    ship_id: i64,
    ship_tid: u32,
    turret_tid: u32,
    ammo_tid: u32,
    base_mod_id: i64,
) -> (SessionState, [i64; 3]) {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String(user.into()), PyRep::String(secret.into())]).await.unwrap();
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.unwrap();
    let guns = fit_combat_ship(ctx, char_id, ship_id, ship_tid, station_id, turret_tid, ammo_tid, base_mod_id);
    let (const_id, reg_id) = ctx.sde.solar_systems.get(&sys_id).map(|s| (s.constellation_id, s.region_id)).unwrap_or((0, 0));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.constellation_id = const_id;
            ch.region_id = reg_id;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
    });
    ctx.sync_session(&mut sess);
    (sess, guns)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn setup_space_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    secret: &str,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_tid: u32,
    turret_tid: u32,
    ammo_tid: u32,
    base_mod_id: i64,
    pos: DVec3,
) -> (SessionState, [i64; 3]) {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String(user.into()), PyRep::String(secret.into())]).await.unwrap();
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.unwrap();
    let guns = fit_combat_ship(ctx, char_id, ship_id, ship_tid, sys_id as i64, turret_tid, ammo_tid, base_mod_id);
    let (const_id, reg_id) = ctx.sde.solar_systems.get(&sys_id).map(|s| (s.constellation_id, s.region_id)).unwrap_or((0, 0));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.constellation_id = const_id;
            ch.region_id = reg_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
    });
    ctx.sync_session(&mut sess);
    let handle = ctx.systems.get_or_spawn(sys_id);
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_tid);
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 180.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
    (sess, guns)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn warp_lock_and_destroy_via_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    beyonce_id: u64,
    sys_id: u32,
    ship_id: i64,
    guns: &[i64; 3],
    target_id: i64,
    warp_dest_id: Option<i64>,
    max_volleys: usize,
) {
    let handle = ctx.systems.get_or_spawn(sys_id);
    if handle.get_ball_snapshot(target_id).await.unwrap().is_none() {
        return;
    }
    if let Some(dest_id) = warp_dest_id {
        let _ = ctx
            .dispatch_rpc(
                sess,
                "beyonce",
                Some(beyonce_id),
                "CmdWarpToStuff",
                &[PyRep::String("item".into()), PyRep::Integer(dest_id), PyRep::Real(0.0)],
                &HashMap::new(),
                None,
            )
            .await;
        for _ in 0..35 {
            let _ = handle.step_manual(1.0).await;
            if handle.get_ball_snapshot(ship_id).await.ok().flatten().is_some_and(|(_, b)| !matches!(b.mode, MovementMode::Warp { .. })) {
                break;
            }
        }
    }
    if let Ok(Some((_, tb))) = handle.get_ball_snapshot(target_id).await {
        let sb_pos = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;
        if sb_pos.distance(tb.position) > 35_000.0 {
            let _ = handle.set_ball_position(ship_id, tb.position + DVec3::new(5_000.0, 0.0, 0.0)).await;
        }
    } else {
        return;
    }
    rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)]).await.expect("AddTarget");
    for _ in 0..45 {
        if handle.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == target_id && l.locked) {
            break;
        }
        let _ = handle.step_manual(1.0).await;
    }
    for _ in 0..max_volleys {
        for &gid in guns {
            if handle.get_ball_snapshot(target_id).await.unwrap().is_none() {
                return;
            }
            rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::Integer(target_id)]).await.expect("dogmaIM.Activate");
        }
        if handle.get_ball_snapshot(target_id).await.unwrap().is_none() {
            return;
        }
        if handle.get_combat_state(ship_id).await.ok().flatten().is_some_and(|cs| cs.capacitor < 300.0) {
            let _ = handle.step_manual(25.0).await;
        }
    }
    assert!(
        handle.get_ball_snapshot(target_id).await.unwrap().is_none(),
        "Target {target_id} in system {sys_id} must be destroyed via dogmaIM.Activate"
    );
}

#[tokio::test]
async fn test_belt_rats_clone_soldier_mordu_and_officer_spawns() {
    // parity: feat:npc.belt-rats, feat:npc.clone-soldier-spawns, feat:npc.officer-npc
    eve_testkit::attest(&[
        "feat:npc.belt-rats",
        "feat:npc.clone-soldier-spawns",
        "feat:npc.officer-npc",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // 1. Beltless High-Sec (`30_000_142` Jita): undocked pilot + cluster tick -> 0 belt rats, 0 Clone Soldiers, 0 Officers
    let (mut jita_sess, _) = setup_docked_pilot(&ctx, 7101, "demo", "demo-secret", 90_000_001, 30_000_142, 60_003_760, 910_001, 639, 2961, 21894, 920_000).await;
    rpc(&ctx, &mut jita_sess, "stationSvc", "Undock", &[]).await.expect("Undock Jita");
    tick_background_cluster(&ctx).await;
    let jita_rats = ctx.npc_spawns.read().unwrap().get(&30_000_142).map(|m| m.entities.len()).unwrap_or(0);
    assert_eq!(jita_rats, 0, "Beltless Jita must spawn 0 belt rats");
    assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| n.solar_system_id == 30_000_142 && matches!(n.archetype.as_str(), "CloneSoldier" | "Officer" | "MorduCommander"))));

    // 2. High-Sec Belt System (`30_000_001` Tanoo, `0.858` sec): spawns frigate/destroyer belt rats, 0 Clone Soldiers, 0 Officers
    let tanoo_stn = ctx.sde.solar_systems[&30_000_001].stations[0].station_id;
    let (mut tanoo_sess, _) = setup_docked_pilot(&ctx, 7102, "other", "other-secret", 90_000_002, 30_000_001, tanoo_stn, 910_002, 639, 2961, 21894, 920_100).await;
    rpc(&ctx, &mut tanoo_sess, "stationSvc", "Undock", &[]).await.expect("Undock Tanoo");
    tick_background_cluster(&ctx).await;
    let tanoo_groups: Vec<u32> = ctx.npc_spawns.read().unwrap()[&30_000_001].entities.values().map(|e| e.spawn_group_id).collect();
    assert!(!tanoo_groups.is_empty(), "High-sec system with belts must spawn regional belt rats");
    for sg in &tanoo_groups {
        assert!(ctx.sde.npc_spawn_groups[sg].ship_class <= 2, "High-sec belt rats must be Frigate/Destroyer class (<= 2)");
    }
    assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| n.solar_system_id == 30_000_001 && matches!(n.archetype.as_str(), "CloneSoldier" | "MorduCommander"))));

    // 3. Low-Sec Belt System (`30_000_012` Asabona, `0.323` sec): Clone Soldier + Mordu's Legion + Faction Commander
    let low_sys = 30_000_012u32;
    let low_stn = ctx.sde.solar_systems[&low_sys].stations[0].station_id;
    let low_belt_id = ctx.sde.celestials_in_system(low_sys).into_iter().find(|c| c.group_id == 9).unwrap().celestial_id as i64;
    let (mut low_sess, low_guns) = setup_docked_pilot(&ctx, 7103, "demo", "demo-secret", 90_000_001, low_sys, low_stn, 910_003, 54733, 54754, 54783, 920_200).await;
    rpc(&ctx, &mut low_sess, "stationSvc", "Undock", &[]).await.expect("Undock Asabona");
    let low_beyonce = bind_beyonce(&ctx, &mut low_sess, low_sys).await;

    let hit_seed = find_multi_hit_seed(&ctx, 0.08, 4);
    ctx.set_mechanic_rng_seed(Some(hit_seed));
    tick_background_cluster(&ctx).await;

    let (cs_bid, mordu_bid, cmd_bid) = with_combat_runtime(&ctx, |rt| {
        let cs = rt.npc_entities.values().find(|n| n.solar_system_id == low_sys && n.archetype == "CloneSoldier").expect("CloneSoldier spawned").ball_id;
        let md = rt.npc_entities.values().find(|n| n.solar_system_id == low_sys && n.archetype == "MorduCommander" && n.faction_id == 500_018).expect("Mordu Special Warfare Unit spawned").ball_id;
        let cm = rt.npc_entities.values().find(|n| n.solar_system_id == low_sys && n.archetype == "Officer").expect("Faction Commander spawned").ball_id;
        (cs, md, cm)
    });

    warp_lock_and_destroy_via_rpc(&ctx, &mut low_sess, low_beyonce, low_sys, 910_003, &low_guns, cs_bid, Some(low_belt_id), 15).await;
    warp_lock_and_destroy_via_rpc(&ctx, &mut low_sess, low_beyonce, low_sys, 910_003, &low_guns, mordu_bid, None, 15).await;
    warp_lock_and_destroy_via_rpc(&ctx, &mut low_sess, low_beyonce, low_sys, 910_003, &low_guns, cmd_bid, None, 20).await;

    ctx.db.with_snapshot(|snap| {
        let low_wrecks: Vec<i64> = snap.items.values().filter(|i| i.location_id == low_sys as i64 && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186)).map(|i| i.item_id).collect();
        let wreck_loot: Vec<&InventoryItemRecord> = snap.items.values().filter(|i| low_wrecks.contains(&i.location_id)).collect();
        assert!(wreck_loot.iter().any(|i| ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 1206)), "Clone Soldier wreck must drop SDE Security Tag (group 1206)");
        assert!(wreck_loot.iter().any(|i| matches!(i.type_id, 33817 | 33819 | 33821) && i.is_blueprint_copy && i.blueprint_runs_remaining == Some(1)), "Mordu Commander wreck must drop 1-run Garmur/Orthrus/Barghest BPC");
        assert!(wreck_loot.iter().any(|i| ctx.sde.types.get(&i.type_id).is_some_and(|t| t.meta_group_id == Some(4))), "Faction Commander wreck must drop SDE Faction module (meta_group 4)");
    });

    tick_background_cluster(&ctx).await;
    assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| n.solar_system_id == low_sys && n.archetype == "CloneSoldier")), "Clone Soldier must respect 20m respawn cooldown");
    let (paid_isk, _, _) = flush_npc_bounty_tick(&ctx, low_sys, 90_000_001, 0.0);
    assert!(paid_isk > 0.0, "20m bounty tick must pay pilot ISK for low-sec NPC kills");
    ctx.advance_secs(1_205);
    tick_background_cluster(&ctx).await;
    assert!(with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| n.solar_system_id == low_sys && n.archetype == "CloneSoldier")), "Clone Soldier must respawn after 20m cooldown");

    // 4. True Null-Sec (`30_001_269` H-PA29, `-0.241` sec): Named Guristas Officer (`group 564`) + `meta_group 5` Officer module drop
    // Isolate in a fresh ServerContext so HashMap iteration order across prior systems cannot shift RNG draws
    let ctx4 = ServerContext::new();
    common::pin_clock(&ctx4);
    let null_sys = 30_001_269u32;
    let null_belt = ctx4.sde.celestials_in_system(null_sys).into_iter().find(|c| c.group_id == 9).unwrap();
    let (mut null_sess, null_guns) = setup_space_pilot(&ctx4, 7104, "other", "other-secret", 90_000_002, null_sys, 910_004, 54733, 54754, 54783, 920_300, null_belt.position + DVec3::new(4_000.0, 0.0, 0.0)).await;
    let null_beyonce = bind_beyonce(&ctx4, &mut null_sess, null_sys).await;
    // Seed 1_603_003 yields 6 consecutive draws < 0.10, guaranteeing the null-sec Officer spawn roll (0.10) hits
    let officer_seed = 1_603_003u64;
    ctx4.set_mechanic_rng_seed(Some(officer_seed));
    tick_background_cluster(&ctx4).await;

    let (off_bid, off_tid) = with_combat_runtime(&ctx4, |rt| {
        let off = rt.npc_entities.values().find(|n| n.solar_system_id == null_sys && ctx4.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 564)).expect("Named Guristas Officer (group 564) spawned in null-sec");
        (off.ball_id, off.type_id)
    });
    assert!(ctx4.sde.types[&off_tid].default_attributes.get(&481).copied().unwrap_or(0.0) >= 1_000_000.0);
    warp_lock_and_destroy_via_rpc(&ctx4, &mut null_sess, null_beyonce, null_sys, 910_004, &null_guns, off_bid, Some(null_belt.celestial_id as i64), 35).await;

    let has_officer_mod = ctx4.db.with_snapshot(|snap| {
        let wrecks: Vec<i64> = snap.items.values().filter(|i| i.location_id == null_sys as i64 && ctx4.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186)).map(|i| i.item_id).collect();
        snap.items.values().any(|i| wrecks.contains(&i.location_id) && ctx4.sde.types.get(&i.type_id).is_some_and(|t| t.meta_group_id == Some(5)))
    });
    assert!(has_officer_mod, "Destroyed Null-Sec Officer must drop authentic SDE Officer module (meta_group 5)");
}

#[tokio::test]
async fn test_drifters_overshield_lux_kontos_doomsday_and_wreck() {
    // parity: feat:npc.drifters
    eve_testkit::attest(&["feat:npc.drifters"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let wh_sys = 31_000_007u32;
    let anchor = ctx.sde.celestials_in_system(wh_sys).first().map(|c| c.position).unwrap_or(DVec3::ZERO) + DVec3::new(15_000.0, 0.0, 0.0);
    let (mut sess, guns) = setup_space_pilot(&ctx, 7201, "demo", "demo-secret", 90_000_001, wh_sys, 910_101, 54733, 54754, 54783, 921_000, anchor + DVec3::new(6_000.0, 0.0, 0.0)).await;
    let beyonce_id = bind_beyonce(&ctx, &mut sess, wh_sys).await;

    tick_background_cluster(&ctx).await;

    let (cruiser_bid, bs_bid) = with_combat_runtime(&ctx, |rt| {
        let c = rt.npc_entities.values().find(|n| n.solar_system_id == wh_sys && n.archetype == "Drifter" && ctx.sde.types[&n.type_id].group_id == 1956).expect("Drifter Cruiser spawned");
        let b = rt.npc_entities.values().find(|n| n.solar_system_id == wh_sys && n.archetype == "Drifter" && ctx.sde.types[&n.type_id].group_id == 1310).expect("Drifter Battleship spawned");
        assert_eq!(c.drifter_overshield_hp, 0.0);
        assert_eq!(b.drifter_overshield_hp, 4_000.0);
        assert!(!b.drifter_doomsday_fired);
        (c.ball_id, b.ball_id)
    });

    let handle = ctx.systems.get_or_spawn(wh_sys);
    rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(bs_bid)]).await.expect("Lock Drifter BS");
    for _ in 0..35 {
        if handle.get_target_locks(910_101).await.unwrap().iter().any(|l| l.target_id == bs_bid && l.locked) {
            break;
        }
        let _ = handle.step_manual(1.0).await;
    }

    // Fire 1 Vorton volley via dogmaIM.Activate to break the 4,000 HP overshield while leaving the Drifter BS alive
    for &gid in &guns {
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::Integer(bs_bid)]).await.expect("Activate gun on Drifter BS");
    }
    let ship_hp_before_dd = {
        let cs = handle.get_combat_state(910_101).await.unwrap().unwrap();
        cs.shield_hp + cs.armor_hp + cs.structure_hp
    };

    tick_background_cluster(&ctx).await;
    let (ov_after, dd_fired, cruiser_dd) = with_combat_runtime(&ctx, |rt| {
        let b = &rt.npc_entities[&bs_bid];
        let c = &rt.npc_entities[&cruiser_bid];
        (b.drifter_overshield_hp, b.drifter_doomsday_fired, c.drifter_doomsday_fired)
    });
    assert_eq!(ov_after, 0.0, "Drifter overshield must deplete to 0.0 under weapon fire");
    assert!(dd_fired, "Drifter Battleship must fire Lux Kontos Doomsday upon overshield depletion");
    assert!(!cruiser_dd, "Drifter Cruiser must never fire Lux Kontos Doomsday");

    let ship_hp_after_dd = {
        let cs = handle.get_combat_state(910_101).await.unwrap().unwrap();
        cs.shield_hp + cs.armor_hp + cs.structure_hp
    };
    assert!(ship_hp_before_dd - ship_hp_after_dd >= 4_500.0, "Lux Kontos Doomsday must deal massive omni damage to target tank");

    warp_lock_and_destroy_via_rpc(&ctx, &mut sess, beyonce_id, wh_sys, 910_101, &guns, bs_bid, None, 35).await;
    let has_drifter_wreck = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| {
            i.location_id == wh_sys as i64
                && (matches!(i.type_id, 34768 | 37531 | 56741)
                    || ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186 && t.name.contains("Drifter")))
        })
    });
    assert!(has_drifter_wreck, "Destroyed Drifter Battleship must spawn SDE Drifter Wreck in space");
}

pub(crate) mod npc_mining_ops_hauler_convoy {
    // Part 2 of V24.01 NPC & PvE closure integration tests:
    // Living World NPC Mining Ops & Hauler Convoys, CONCORD & Faction Police enforcement,
    // and SDE-backed Pirate Strongholds, Homefront Operations, and Triglavian Sites.


    use crate::common;
    use crate::npc_spawns_and_encounters::{
        bind_beyonce, rpc, setup_docked_pilot, setup_space_pilot, warp_lock_and_destroy_via_rpc,
    };
    use eve_destiny::DVec3;
    use eve_proto::PyRep;
    use eve_server::living_world::NpcArchetype;
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::with_combat_runtime;
    use eve_server::services::insurgency_services::{
        ensure_stronghold_in_space, init_insurgency_campaign, FACTION_GURISTAS,
        FORECASTING_DURATION_SECS, PHASE_FORECASTING, PHASE_WINDDOWN, PIRATE_STRONGHOLD_WRECK_TYPE,
    };
    use eve_server::services::pve_special_sites::{ensure_special_sites_spawned, with_pve_special_reg};
    use eve_server::session_cluster_lifecycle::tick_background_cluster;
    use std::collections::HashMap;

    fn sum_sell_order_volume(orders_rep: &PyRep) -> f64 {
        let PyRep::Tuple(tup) = orders_rep else {
            panic!("expected Tuple from marketProxy.GetOrders");
        };
        tup[0]
            .crowset_rows()
            .expect("sell crowset")
            .iter()
            .filter_map(|r| r.dbrow_get("volRemaining").and_then(|v| v.as_f64()))
            .sum()
    }

    #[tokio::test]
    async fn test_npc_mining_ops_and_hauler_convoy_lifecycle() {
        // parity: feat:npc.npc-hauler, feat:npc.mining-op-npc, feat:mining.npc-mining-ops
        eve_testkit::attest(&[
            "feat:npc.npc-hauler",
            "feat:npc.mining-op-npc",
            "feat:mining.npc-mining-ops",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let sys_id = 30_000_001u32; // Tanoo (high-sec with asteroid belts, NPC station, and stargates)
        let stn_id = ctx.sde.solar_systems[&sys_id].stations[0].station_id;
        let belt_pos = ctx
            .sde
            .celestials_in_system(sys_id)
            .into_iter()
            .find(|c| c.group_id == 9)
            .unwrap()
            .position;

        let (mut sess, guns) = setup_docked_pilot(
            &ctx, 7301, "demo", "demo-secret", 90_000_001, sys_id, stn_id, 910_201, 54733, 54754,
            54783, 922_000,
        )
        .await;
        rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
            .await
            .expect("Undock Tanoo");
        rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "SetSafetyLevel",
            &[PyRep::Integer(0)],
        )
        .await
        .expect("SetSafetyLevel Red");

        let tanoo_handle = ctx.systems.get_or_spawn(sys_id);
        let init_asts = tanoo_handle.populate_asteroid_belt_entities().await.unwrap();
        let init_total_units: u64 = init_asts.iter().map(|a| a.remaining_units).sum();

        let init_orders = rpc(
            &ctx,
            &mut sess,
            "marketProxy",
            "GetOrders",
            &[PyRep::Integer(1230)],
        )
        .await
        .expect("GetOrders Veldspar");
        let init_sell_vol = sum_sell_order_volume(&init_orders);

        // Tick 1: spawns NPC Venture (32880) + racial Industrial at the belt, and HaulerConvoy in transit
        tick_background_cluster(&ctx).await;

        // Advance 60s and tick: Venture completes 60s mining cycle (1,600 units) and HaulerConvoy jumps stargate (> 15s transit)!
        ctx.advance_secs(60);
        tick_background_cluster(&ctx).await;

        let convoys = ctx.living_world.list_convoys();
        let mining_op = convoys
            .iter()
            .find(|c| c.archetype == NpcArchetype::MiningOp && c.current_system_id == sys_id)
            .expect("NPC MiningOp spawned in Tanoo");
        let hauler_convoy = convoys
            .iter()
            .find(|c| c.archetype == NpcArchetype::HaulerConvoy && c.origin_system_id == sys_id)
            .expect("NPC HaulerConvoy spawned from Tanoo");
        assert_eq!(mining_op.ship_type_id, 32880);
        assert_eq!(
            mining_op.cargo_quantity, 1_600,
            "SDE Venture + 2x Miner I must extract 1,600 Veldspar units (160 m3) on tick 1"
        );
        assert_ne!(
            hauler_convoy.current_system_id, sys_id,
            "HaulerConvoy must jump stargate to connected system on tick"
        );

        let (_, miner_ball) = tanoo_handle
            .get_ball_snapshot(mining_op.convoy_id)
            .await
            .unwrap()
            .expect("NPC Venture ball in belt");
        assert!(
            miner_ball.position.distance(belt_pos) <= 50_000.0,
            "NPC Venture must be spawned on the asteroid belt grid"
        );
        assert!(
            tanoo_handle
                .get_ball_snapshot(mining_op.convoy_id + 1_000_000)
                .await
                .unwrap()
                .is_some(),
            "Companion NPC Industrial hauler ball on belt grid"
        );

        // Advance 33 more ticks (each 20,000-unit rock takes 12 full + 1 partial 800-unit tick -> 33 ticks = 51,200 units >= 50,000 Venture ore hold)!
        for _ in 0..33 {
            ctx.advance_secs(60);
            tick_background_cluster(&ctx).await;
        }
        let post_asts = tanoo_handle.populate_asteroid_belt_entities().await.unwrap();
        let post_total_units: u64 = post_asts.iter().map(|a| a.remaining_units).sum();
        assert!(
            init_total_units - post_total_units >= 50_000,
            "Belt asteroids must deplete over ticks by >= 50,000 units"
        );

        let post_orders = rpc(
            &ctx,
            &mut sess,
            "marketProxy",
            "GetOrders",
            &[PyRep::Integer(1230)],
        )
        .await
        .expect("GetOrders Veldspar after haul");
        let post_sell_vol = sum_sell_order_volume(&post_orders);
        assert!(
            post_sell_vol >= init_sell_vol + 50_000.0,
            "NPC mining op haul must replenish regional Veldspar sell volume by >= 50,000 units via marketProxy.GetOrders"
        );

        // Pursue an organically spawned HaulerConvoy via CmdStargateJump and destroy via dogmaIM.Activate!
        let active_hauler = ctx
            .living_world
            .list_convoys()
            .into_iter()
            .find(|c| {
                c.archetype == NpcArchetype::HaulerConvoy
                    && !c.destroyed
                    && !ctx.living_world.is_convoy_docked(c.convoy_id)
            })
            .unwrap_or_else(|| {
                let cid = ctx.living_world.ensure_system_hauler_convoy(sys_id, &ctx.sde);
                ctx.living_world.get_convoy(cid).unwrap()
            });
        if active_hauler.current_system_id == sys_id {
            ctx.advance_secs(15);
            tick_background_cluster(&ctx).await;
        }
        let jumped_hauler = ctx.living_world.get_convoy(active_hauler.convoy_id).unwrap();
        let dest_sys = jumped_hauler.current_system_id;
        let gate = ctx
            .sde
            .stargates()
            .values()
            .find(|g| g.from_solar_system_id == sys_id && g.to_solar_system_id == dest_sys)
            .expect("Connecting stargate");
        let _ = tanoo_handle
            .set_ball_position(910_201, gate.position + DVec3::new(1_000.0, 0.0, 0.0))
            .await;
        ctx.advance_secs(15);
        let tanoo_beyonce = bind_beyonce(&ctx, &mut sess, sys_id).await;
        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(tanoo_beyonce),
            "CmdStargateJump",
            &[PyRep::Integer(gate.stargate_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CmdStargateJump to pursue HaulerConvoy");
        ctx.advance_secs(15);

        let dest_beyonce = bind_beyonce(&ctx, &mut sess, dest_sys).await;
        warp_lock_and_destroy_via_rpc(
            &ctx,
            &mut sess,
            dest_beyonce,
            dest_sys,
            910_201,
            &guns,
            jumped_hauler.convoy_id,
            None,
            15,
        )
        .await;
        assert!(
            ctx.living_world.get_convoy(jumped_hauler.convoy_id).unwrap().destroyed,
            "HaulerConvoy must be marked destroyed when killed in space"
        );
        let dropped_trit = ctx.db.with_snapshot(|snap| {
            snap.items.values().any(|i| {
                i.type_id == 34
                    && i.quantity >= 25_000
                    && snap
                        .items
                        .get(&i.location_id)
                        .is_some_and(|w| w.location_id == dest_sys as i64)
            })
        });
        assert!(
            dropped_trit,
            "Destroyed HaulerConvoy must drop its 25,000 Tritanium cargo into a space wreck"
        );
    }

    #[tokio::test]
    async fn test_concord_response_and_faction_police_enforcement() {
        // parity: feat:npc.concord-npc, feat:npc.navy-npc
        eve_testkit::attest(&[
            "feat:npc.concord-npc",
            "feat:npc.navy-npc",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let jita = 30_000_142u32;
        let jita_stn = 60_003_760i64;

        let (mut att_sess, att_guns) = setup_docked_pilot(
            &ctx, 7401, "demo", "demo-secret", 90_000_001, jita, jita_stn, 910_301, 639, 2961, 21894,
            923_000,
        )
        .await;
        let (mut vic_sess, _) = setup_docked_pilot(
            &ctx, 7402, "other", "other-secret", 90_000_002, jita, jita_stn, 910_302, 587, 2961, 21894,
            923_100,
        )
        .await;
        // Victim leaves ship in station to undock in capsule (670) so 3 unlawful shots organically drive attacker sec status <= -5.0!
        ctx.advance_secs(15);
        rpc(
            &ctx,
            &mut vic_sess,
            "ship",
            "LeaveShip",
            &[PyRep::Integer(910_302)],
        )
        .await
        .expect("Victim boards capsule");
        ctx.advance_secs(15);
        rpc(&ctx, &mut att_sess, "stationSvc", "Undock", &[])
            .await
            .expect("Attacker undocks");
        rpc(&ctx, &mut vic_sess, "stationSvc", "Undock", &[])
            .await
            .expect("Victim capsule undocks");
        let vic_pod_id = ctx.db.get_character(90_000_002).unwrap().active_ship_id;
        assert_eq!(ctx.db.with_snapshot(|s| s.items[&vic_pod_id].type_id), 670);
        ctx.advance_secs(35);

        // Lawful pilots in Jita trigger ZERO Faction Police on cluster tick
        tick_background_cluster(&ctx).await;
        assert!(!with_combat_runtime(&ctx, |rt| {
            rt.npc_entities
                .values()
                .any(|n| n.solar_system_id == jita && n.archetype == "FactionPolice")
        }));

        // 1. Unlawful high-sec capsule aggression via dogmaIM.Activate -> Criminal flag + sec status penalty (once per target + destruction) + 6s CONCORD response!
        rpc(
            &ctx,
            &mut att_sess,
            "crimewatch",
            "SetSafetyLevel",
            &[PyRep::Integer(0)],
        )
        .await
        .unwrap();
        let jita_handle = ctx.systems.get_or_spawn(jita);
        rpc(
            &ctx,
            &mut att_sess,
            "dogmaIM",
            "AddTarget",
            &[PyRep::Integer(vic_pod_id)],
        )
        .await
        .unwrap();
        for _ in 0..30 {
            if jita_handle
                .get_target_locks(910_301)
                .await
                .unwrap()
                .iter()
                .any(|l| l.target_id == vic_pod_id && l.locked)
            {
                break;
            }
            let _ = jita_handle.step_manual(1.0).await;
        }
        for &gid in &att_guns {
            rpc(
                &ctx,
                &mut att_sess,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(gid), PyRep::Integer(vic_pod_id)],
            )
            .await
            .expect("Unlawful high-sec pod shot");
        }

        ctx.advance_secs(2);
        tick_background_cluster(&ctx).await;
        assert_eq!(
            ctx.db.get_character(90_000_001).unwrap().active_ship_id,
            910_301,
            "CONCORD must respect 6s Jita arrival delay"
        );

        ctx.advance_secs(6);
        tick_background_cluster(&ctx).await;
        ctx.sync_session(&mut att_sess);
        let reports = ctx.crimewatch.concord_reports();
        let rep = reports
            .iter()
            .rfind(|r| r.criminal_char_id == 90_000_001)
            .expect("CONCORD execution report emitted");
        assert!(
            rep.ecm_jammed
                && rep.webbed_speed_fraction == 0.0
                && rep.warp_scramble_strength >= 100
                && rep.capacitor_after_neut == 0.0,
            "CONCORD must apply ECM jam, 100% web, warp scram, and cap drain"
        );

        // Verify CONCORD Police Commander (11957) ball is spawned in Jita and 100% invulnerable to damage
        let (_, concord_ball) = jita_handle
            .get_ball_snapshot(rep.concord_ball_id)
            .await
            .unwrap()
            .expect("CONCORD ball in space");
        assert_eq!(concord_ball.type_id, 11_957);
        let concord_cs0 = jita_handle
            .get_combat_state(rep.concord_ball_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            concord_cs0.shield_resonance,
            [0.0; 4],
            "CONCORD ship must have 100% invulnerable shield resonance"
        );

        let att_after = ctx.db.get_character(90_000_001).unwrap();
        assert_ne!(att_after.active_ship_id, 910_301, "CONCORD must destroy the criminal ship");
        assert_eq!(
            ctx.db.with_snapshot(|s| s.items[&att_after.active_ship_id].type_id),
            670,
            "Criminal pilot must be ejected into capsule (670)"
        );
        assert!(
            att_after.security_status < -1.0,
            "Unlawful pod aggression must drop security status once per target (no repeat-shot/CONCORD double-deduction), got {}",
            att_after.security_status
        );

        let kr_rep = rpc(&ctx, &mut vic_sess, "bountyProxy", "GetMyKillRights", &[])
            .await
            .expect("GetMyKillRights");
        assert!(
            format!("{kr_rep:?}").contains("90000001"),
            "Victim must receive private kill right against criminal attacker"
        );

        // 2. Outlaw (`<= -5.0` sec) Faction Police (`feat:npc.navy-npc`): capsules never engaged & allowed to dock; non-capsule ship engaged & refused docking!
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&90_000_001) {
                ch.security_status = -5.5;
            }
        });
        ctx.advance_secs(920);
        tick_background_cluster(&ctx).await;
        assert!(
            !with_combat_runtime(&ctx, |rt| {
                rt.npc_entities.values().any(|n| {
                    n.solar_system_id == jita
                        && n.archetype == "FactionPolice"
                        && n.target_id == Some(att_after.active_ship_id)
                })
            }),
            "Capsules (670) at <= -5.0 security status must NEVER be engaged by Faction Police"
        );
        let att_beyonce = bind_beyonce(&ctx, &mut att_sess, jita).await;
        let stn_pos = ctx.sde.solar_systems[&jita]
            .stations
            .iter()
            .find(|s| s.station_id == jita_stn)
            .unwrap()
            .position;
        let _ = jita_handle
            .set_ball_position(att_after.active_ship_id, stn_pos + DVec3::new(200.0, 0.0, 0.0))
            .await;
        ctx.dispatch_rpc(
            &mut att_sess,
            "beyonce",
            Some(att_beyonce),
            "CmdDock",
            &[PyRep::Integer(jita_stn), PyRep::Integer(att_after.active_ship_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("Outlaw capsule is allowed to dock");

        let (mut outlaw_sess, _) = setup_docked_pilot(
            &ctx, 7403, "demo", "demo-secret", 90_000_001, jita, jita_stn, 910_305, 587, 2961, 21894,
            923_200,
        )
        .await;
        ctx.advance_secs(15);
        rpc(&ctx, &mut outlaw_sess, "stationSvc", "Undock", &[])
            .await
            .expect("Outlaw ship undocks");
        let outlaw_beyonce = bind_beyonce(&ctx, &mut outlaw_sess, jita).await;
        tick_background_cluster(&ctx).await;
        assert!(
            with_combat_runtime(&ctx, |rt| {
                rt.npc_entities.values().any(|n| {
                    n.solar_system_id == jita
                        && n.archetype == "FactionPolice"
                        && n.target_id == Some(910_305)
                })
            }),
            "Outlaw non-capsule ship in high-sec must be targeted by Faction Police"
        );
        let _ = jita_handle
            .set_ball_position(910_305, stn_pos + DVec3::new(200.0, 0.0, 0.0))
            .await;
        let dock_err = ctx
            .dispatch_rpc(
                &mut outlaw_sess,
                "beyonce",
                Some(outlaw_beyonce),
                "CmdDock",
                &[PyRep::Integer(jita_stn), PyRep::Integer(910_305)],
                &HashMap::new(),
                None,
            )
            .await
            .expect_err("Outlaw non-capsule ship must be refused high-sec docking");
        assert!(!dock_err.code.is_empty());
    }

    #[tokio::test]
    async fn test_sde_backed_pirate_strongholds_homefront_and_triglavian_sites() {
        // parity: feat:pve.pirate-strongholds, feat:pve.triglavian-sites, feat:pve.homefront-ops
        eve_testkit::attest(&[
            "feat:pve.pirate-strongholds",
            "feat:pve.triglavian-sites",
            "feat:pve.homefront-ops",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        ensure_special_sites_spawned(&ctx).await;

        // 1. Homefront Operations (`feat:pve.homefront-ops`): SDE archetype 70 in high-sec Jita vs empty in null-sec; boss kill via dogmaIM.Activate
        let jita = 30_000_142u32;
        let (mut sess, guns) = setup_docked_pilot(
            &ctx, 7501, "demo", "demo-secret", 90_000_001, jita, 60_003_760, 910_401, 54733, 54754,
            54783, 924_000,
        )
        .await;
        rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.unwrap();
        let jita_beyonce = bind_beyonce(&ctx, &mut sess, jita).await;

        let PyRep::Dict(null_hf) = rpc(
            &ctx,
            &mut sess,
            "dungeonInstanceCacheMgr",
            "GetHomefrontSiteInstances",
            &[PyRep::Integer(30_001_269)],
        )
        .await
        .unwrap() else {
            panic!()
        };
        let PyRep::Dict(null_dngs) = &null_hf[0].1 else {
            panic!()
        };
        assert!(null_dngs.is_empty(), "Null-sec system must have 0 Homefront Operation instances");

        let PyRep::Dict(jita_hf) = rpc(
            &ctx,
            &mut sess,
            "dungeonInstanceCacheMgr",
            "GetHomefrontSiteInstances",
            &[PyRep::Integer(jita as i64)],
        )
        .await
        .unwrap() else {
            panic!()
        };
        let PyRep::Dict(jita_dngs) = &jita_hf[0].1 else {
            panic!()
        };
        let (hf_did, PyRep::List(hf_insts)) = (&jita_dngs[0].0, &jita_dngs[0].1) else {
            panic!()
        };
        assert_eq!(ctx.sde.dungeons[&(hf_did.as_i64().unwrap() as u32)].archetype_id, Some(70));
        let hf_iid = hf_insts[0].as_i64().unwrap();
        let hf_boss_bid = with_pve_special_reg(&ctx, |reg| {
            reg.homefront_sites[&jita]
                .iter()
                .find(|s| s.instance_id == hf_iid)
                .unwrap()
                .boss_npc_ball_id
        });

        let bal_before_hf = ctx.db.get_character(90_000_001).unwrap().balance.0;
        warp_lock_and_destroy_via_rpc(&ctx, &mut sess, jita_beyonce, jita, 910_401, &guns, hf_boss_bid, None, 25).await;
        assert!(
            ctx.db.get_character(90_000_001).unwrap().balance.0 > bal_before_hf,
            "Killing Homefront boss via dogmaIM.Activate must award CorporateRewardPayout ISK"
        );

        // 2. Triglavian Sites (`feat:pve.triglavian-sites`): Gammel (30003076) Striking Leshak (48122) killed via dogmaIM.Activate -> Survey DB (48121) + dual standings
        let gammel = 30_003_076u32;
        let (mut trig_sess, trig_guns) = setup_space_pilot(
            &ctx, 7502, "other", "other-secret", 90_000_002, gammel, 910_402, 54733, 54754, 54783,
            924_100, DVec3::ZERO,
        )
        .await;
        let trig_beyonce = bind_beyonce(&ctx, &mut trig_sess, gammel).await;
        let PyRep::Dict(g_rep) = rpc(
            &ctx,
            &mut trig_sess,
            "dungeonInstanceCacheMgr",
            "GetTriglavianSiteInstances",
            &[PyRep::Integer(gammel as i64)],
        )
        .await
        .unwrap() else {
            panic!()
        };
        let PyRep::Dict(g_dngs) = &g_rep[0].1 else {
            panic!()
        };
        assert_eq!(g_dngs.len(), 2);
        let trig_boss_bid = with_pve_special_reg(&ctx, |reg| reg.triglavian_sites[&gammel][0].boss_npc_ball_id);

        warp_lock_and_destroy_via_rpc(&ctx, &mut trig_sess, trig_beyonce, gammel, 910_402, &trig_guns, trig_boss_bid, None, 35).await;
        assert!(
            ctx.db.with_snapshot(|s| s.items.values().any(|i| i.owner_id == 90_000_002 && i.type_id == 48_121)),
            "Triglavian boss kill must award Survey Database (48121)"
        );
        let ch2 = ctx.db.get_character(90_000_002).unwrap();
        assert!(
            ch2.standings[&500_027] > 0.0 && ch2.standings[&500_026] < 0.0,
            "Killing Triglavian boss must shift EDENCOM (+0.15) and Triglavian (-0.15) standings"
        );

        // 3. Pirate Strongholds (`feat:pve.pirate-strongholds`): Forecasting invulnerability via dogmaIM.Activate -> clock advance to Active -> destruction & WindDown
        let fob_sys = 30_002_048u32;
        init_insurgency_campaign(&ctx, 901, FACTION_GURISTAS, 500_001, fob_sys, vec![gammel]);
        let sh_id = ensure_stronghold_in_space(&ctx, 901).await.expect("Stronghold spawned");
        let (mut fob_sess, fob_guns) = setup_space_pilot(
            &ctx, 7503, "demo", "demo-secret", 90_000_001, fob_sys, 910_403, 23773, 41160, 20811,
            924_200, DVec3::new(60_000.0, 0.0, 50_000.0),
        )
        .await;
        let fob_beyonce = bind_beyonce(&ctx, &mut fob_sess, fob_sys).await;
        let fob_handle = ctx.systems.get_or_spawn(fob_sys);

        let PyRep::Tuple(fob_snap0) = rpc(&ctx, &mut fob_sess, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[])
            .await
            .unwrap() else {
            panic!()
        };
        assert_eq!(fob_snap0[1].dict_get("phase"), Some(&PyRep::String(PHASE_FORECASTING.into())));

        rpc(&ctx, &mut fob_sess, "dogmaIM", "AddTarget", &[PyRep::Integer(sh_id)]).await.unwrap();
        for _ in 0..35 {
            if fob_handle.get_target_locks(910_403).await.unwrap().iter().any(|l| l.target_id == sh_id && l.locked) {
                break;
            }
            let _ = fob_handle.step_manual(1.0).await;
        }
        let sh_hp0 = fob_handle.get_combat_state(sh_id).await.unwrap().unwrap().shield_hp;
        rpc(&ctx, &mut fob_sess, "dogmaIM", "Activate", &[PyRep::Integer(fob_guns[0]), PyRep::Integer(sh_id)]).await.unwrap();
        assert_eq!(fob_handle.get_combat_state(sh_id).await.unwrap().unwrap().shield_hp, sh_hp0, "Stronghold must be invulnerable during Forecasting phase");

        ctx.advance_secs(FORECASTING_DURATION_SECS + 60);
        rpc(&ctx, &mut fob_sess, "dogmaIM", "Activate", &[PyRep::Integer(fob_guns[0]), PyRep::Integer(sh_id)]).await.unwrap();
        assert!(fob_handle.get_combat_state(sh_id).await.unwrap().unwrap().shield_hp < sh_hp0, "Stronghold must take real weapon damage in Active phase");

        warp_lock_and_destroy_via_rpc(&ctx, &mut fob_sess, fob_beyonce, fob_sys, 910_403, &fob_guns, sh_id, None, 1_500).await;
        let PyRep::List(inst_after) = rpc(&ctx, &mut fob_sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyInstances", &[PyRep::Integer(fob_sys as i64)]).await.unwrap() else { panic!() };
        assert_eq!(inst_after.iter().find(|i| i.dict_get("instanceID") == Some(&PyRep::Integer(sh_id))).unwrap().dict_get("completed"), Some(&PyRep::Bool(true)));
        let PyRep::Tuple(fob_snap1) = rpc(&ctx, &mut fob_sess, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[]).await.unwrap() else { panic!() };
        assert_eq!(fob_snap1[1].dict_get("phase"), Some(&PyRep::String(PHASE_WINDDOWN.into())));
        assert!(fob_handle.ballpark.read().unwrap().balls.values().any(|b| b.type_id == PIRATE_STRONGHOLD_WRECK_TYPE));
    }
}

pub(crate) mod navy_npc_faction_police_response {
    // Integration Tests for behavioral verification NPC & Crimewatch Encounter Mechanics.
    //
    // Covers:
    // - `feat:npc.navy-npc`: Faction police response to outlaw pilots in high-sec, pod safety, and docking refusal.
    // - `feat:npc.npc-standings-aggression`: Standing-based aggression for Empire Navy vs Pirate Belt Rats.
    // - `feat:npc.npc-hauler`: Multi-system living-world convoy stargate route traversal and authentic cargo drops.
    // - `feat:npc.npc-spawn-encounter`: Real damage destruction hook triggering authentic multi-wave progression to site completion.

    use eve_db::{CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents};
    use eve_destiny::{Ball, DamageVector, MovementMode};
    use eve_machonet::SessionState;
    use eve_server::living_world::{LivingWorldConvoy, LivingWorldDirector, NpcArchetype};
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::{
        create_custom_encounter_site,
        destroy_specialized_npc_and_queue_bounty, is_outlaw_ship_docking_refused,
        spawn_specialized_npc_entity, step_extended_npc_ai_sync, with_combat_runtime,
    };
    use glam::DVec3;
    use std::collections::HashMap;

    const SYS_JITA: u32 = 30_000_142; // Highsec (0.95)
    const SYS_PERIMETER: u32 = 30_000_144; // Highsec (0.90)
    const SYS_URLEN: u32 = 30_000_143; // Highsec (0.85)

    async fn register_test_pilot(
        ctx: &ServerContext,
        char_id: i64,
        ship_id: i64,
        ship_type_id: u32,
        solar_system_id: u32,
        sec_status: f64,
        standings: HashMap<i64, f64>,
        pos: DVec3,
    ) -> SessionState {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.insert(
                char_id,
                CharacterRecord {
                    character_id: char_id,
                    user_id: char_id as u64,
                    name: format!("Pilot_{char_id}"),
                    corporation_id: 1_000_002,
                    alliance_id: None,
                    war_faction_id: None,
                    fleet_id: None,
                    autopilot_waypoints: Vec::new(),
                    station_id: None,
                    structure_id: None,
                    solar_system_id,
                    constellation_id: 20_000_020,
                    region_id: 10_000_002,
                    active_ship_id: ship_id,
                    balance: IskCents::from_isk(100_000_000),
                    plex_balance: 0,
                    loyalty_points: HashMap::new(),
                    standings,
                    security_status: sec_status,
                    clone_grade: 2,
                    home_station_id: 60_003_760,
                    race_id: 1,
                    bloodline_id: 1,
                    ancestry_id: 1,
                    school_id: 1,
                    neural: eve_db::NeuralAttributes::default(),
                    skill_points: 5_000_000.0,
                    activity_status: None,
                    free_skill_points: 0.0,
                    last_clone_jump_unix: 0,
                    jump_fatigue_minutes: 0.0,
                    jump_cooldown_until_unix: 0,
                    jump_fatigue_until_unix: 0,
                    description: String::new(),
                    title: String::new(),
                    gender: 1,
                    create_date_unix: 0,
                    version: 1,
                    system_epoch: 1,
                },
            );
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_type_id,
                    owner_id: char_id,
                    location_id: solar_system_id as i64,
                    flag: InventoryFlag::None,
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
        });

        let handle = ctx.systems.get_or_spawn(solar_system_id);
        let ball = Ball {
            id: ship_id,
            type_id: ship_type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 3.0,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let _ = handle
            .spawn_ball_with_combat(ball, ctx.build_type_combat_state(ship_type_id))
            .await;

        let mut sess = SessionState::new(char_id as u64);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(solar_system_id);
        sess
    }

    #[tokio::test]
    async fn test_navy_npc_and_faction_police_response() {
        // parity: feat:npc.navy-npc
        eve_testkit::attest(&["feat:npc.navy-npc"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);

        // 1. Outlaw pilot in high-sec piloting a cruiser
        let outlaw_char_id = 910_001i64;
        let outlaw_ship_id = 810_001i64;
        let _sess_outlaw = register_test_pilot(
            &ctx,
            outlaw_char_id,
            outlaw_ship_id,
            624, // Moa cruiser
            SYS_JITA,
            -6.5, // Well below -5.0 outlaw threshold
            HashMap::new(),
            DVec3::new(10_000.0, 0.0, 0.0),
        )
        .await;

        // 2. Outlaw pilot in a capsule (pod)
        let pod_char_id = 910_002i64;
        let pod_ship_id = 810_002i64;
        let _sess_pod = register_test_pilot(
            &ctx,
            pod_char_id,
            pod_ship_id,
            670, // Capsule
            SYS_JITA,
            -6.5,
            HashMap::new(),
            DVec3::new(20_000.0, 0.0, 0.0),
        )
        .await;

        // 3. Lawful pilot (sec status 0.0)
        let lawful_char_id = 910_003i64;
        let lawful_ship_id = 810_003i64;
        let _sess_lawful = register_test_pilot(
            &ctx,
            lawful_char_id,
            lawful_ship_id,
            624,
            SYS_JITA,
            0.0,
            HashMap::new(),
            DVec3::new(30_000.0, 0.0, 0.0),
        )
        .await;

        // Record initial HP before police engagement
        let jita_handle = ctx.systems.get_or_spawn(SYS_JITA);
        let (init_outlaw_shield, init_pod_struct, init_lawful_shield) = {
            let bp = jita_handle.ballpark.read().unwrap();
            let os = bp.combat_states.get(&outlaw_ship_id).map(|c| c.shield_hp).unwrap();
            let ps = bp.combat_states.get(&pod_ship_id).map(|c| c.structure_hp).unwrap();
            let ls = bp.combat_states.get(&lawful_ship_id).map(|c| c.shield_hp).unwrap();
            (os, ps, ls)
        };

        // Drive police spawning through the production entry/tick path
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;

        // Verify police NPC spawned and targeted outlaw ship
        let police_bids: Vec<i64> = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities
                .values()
                .filter(|n| n.archetype == "FactionPolice" && n.solar_system_id == SYS_JITA)
                .map(|n| n.ball_id)
                .collect()
        });
        assert!(
            !police_bids.is_empty(),
            "Faction Police must spawn through cluster tick when outlaw ship enters highsec"
        );
        let police_bid = police_bids[0];

        with_combat_runtime(&ctx, |rt| {
            let police = rt.npc_entities.get(&police_bid).expect("Police entity");
            assert_eq!(police.archetype, "FactionPolice");
            assert_eq!(police.target_id, Some(outlaw_ship_id));
            assert!(police.aggression_threat.contains_key(&outlaw_ship_id));
            assert!(!police.aggression_threat.contains_key(&pod_ship_id), "Capsule must not be targeted");
            assert!(!police.aggression_threat.contains_key(&lawful_ship_id), "Lawful pilot must not be targeted");
            assert_eq!(police.scram_strength, 2.0);
            assert_eq!(police.web_speed_factor, -50.0);
        });

        // Step AI ticks to apply EWAR and weapons synchronously on ballpark
        eve_server::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai(&ctx, SYS_JITA).await;

        // Assert: outlaw ship takes damage and is webbed; pod and lawful pilot take no damage
        {
            let bp = jita_handle.ballpark.read().unwrap();
            let cur_outlaw_shield = bp.combat_states.get(&outlaw_ship_id).map(|c| c.shield_hp).unwrap();
            let cur_pod_struct = bp.combat_states.get(&pod_ship_id).map(|c| c.structure_hp).unwrap();
            let cur_lawful_shield = bp.combat_states.get(&lawful_ship_id).map(|c| c.shield_hp).unwrap();
            let outlaw_ball = bp.balls.get(&outlaw_ship_id).unwrap();

            assert!(
                cur_outlaw_shield < init_outlaw_shield,
                "Outlaw ship must take weapon damage from Faction Police (was {init_outlaw_shield}, now {cur_outlaw_shield})"
            );
            assert!(
                outlaw_ball.speed_fraction < 1.0,
                "Outlaw ship must be webbed (speed_fraction = {})",
                outlaw_ball.speed_fraction
            );
            assert_eq!(
                cur_pod_struct, init_pod_struct,
                "Outlaw capsule must NOT take damage from Faction Police"
            );
            assert_eq!(
                cur_lawful_shield, init_lawful_shield,
                "Lawful pilot must NOT take damage from Faction Police"
            );
        }

        // Verify docking rules
        assert!(
            is_outlaw_ship_docking_refused(&ctx, SYS_JITA, outlaw_char_id, outlaw_ship_id),
            "Docking must be refused to an outlaw ship (sec <= -5.0) in highsec"
        );
        assert!(
            !is_outlaw_ship_docking_refused(&ctx, SYS_JITA, pod_char_id, pod_ship_id),
            "Outlaw capsules (pods) must NEVER be refused docking"
        );
        assert!(
            !is_outlaw_ship_docking_refused(&ctx, SYS_JITA, lawful_char_id, lawful_ship_id),
            "Lawful pilots must not be refused docking"
        );
    }

    #[tokio::test]
    async fn test_npc_standings_aggression_rules() {
        // parity: feat:npc.npc-standings-aggression
        eve_testkit::attest(&["feat:npc.npc-standings-aggression"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);

        // Pilot 1: Neutral standing (0.0) to Caldari Navy (500001)
        let neutral_char_id = 920_001i64;
        let neutral_ship_id = 820_001i64;
        let mut neutral_standings = HashMap::new();
        neutral_standings.insert(500_001i64, 0.0);
        let _sess_neutral = register_test_pilot(
            &ctx,
            neutral_char_id,
            neutral_ship_id,
            624,
            SYS_JITA,
            0.0,
            neutral_standings,
            DVec3::new(5_000.0, 0.0, 0.0),
        )
        .await;

        // Pilot 2: Hostile standing (-6.0) to Caldari Navy (500001)
        let hostile_char_id = 920_002i64;
        let hostile_ship_id = 820_002i64;
        let mut hostile_standings = HashMap::new();
        hostile_standings.insert(500_001i64, -6.0);
        let _sess_hostile = register_test_pilot(
            &ctx,
            hostile_char_id,
            hostile_ship_id,
            624,
            SYS_JITA,
            0.0,
            hostile_standings,
            DVec3::new(6_000.0, 0.0, 0.0),
        )
        .await;

        // Pilot 3: Friendly standing (+8.0) to Guristas Pirates (500010)
        let pirate_friend_char_id = 920_003i64;
        let pirate_friend_ship_id = 820_003i64;
        let mut pirate_standings = HashMap::new();
        pirate_standings.insert(500_010i64, 8.0);
        let _sess_friend = register_test_pilot(
            &ctx,
            pirate_friend_char_id,
            pirate_friend_ship_id,
            624,
            SYS_JITA,
            0.0,
            pirate_standings,
            DVec3::new(7_000.0, 0.0, 0.0),
        )
        .await;

        // Spawn Empire Navy NPC ship on grid
        let navy_ball_id = spawn_specialized_npc_entity(
            &ctx,
            SYS_JITA,
            10046, // Navy ship
            "Navy",
            500_001,
            DVec3::new(5_500.0, 0.0, 0.0),
        )
        .await;

        // Spawn Pirate Belt Rat NPC ship on grid
        let rat_ball_id = spawn_specialized_npc_entity(
            &ctx,
            SYS_JITA,
            16997, // Guristas Pithi Invader
            "BeltRat",
            500_010,
            DVec3::new(7_500.0, 0.0, 0.0),
        )
        .await;

        // Run extended NPC AI sync step
        step_extended_npc_ai_sync(&ctx, SYS_JITA);

        // Verify standing aggression outcomes
        with_combat_runtime(&ctx, |rt| {
            // Empire Navy rules:
            // Must NOT aggress neutral 0.0 standing pilot
            let navy = rt.npc_entities.get(&navy_ball_id).expect("Navy entity");
            assert!(
                !navy.aggression_threat.contains_key(&neutral_ship_id),
                "Empire Navy must NOT aggress pilot with 0.0 standing"
            );
            // MUST aggress <= -5.0 standing pilot
            assert!(
                navy.aggression_threat.contains_key(&hostile_ship_id),
                "Empire Navy MUST aggress pilot with <= -5.0 standing to empire faction"
            );

            // Pirate Belt Rat rules:
            // Belt rats aggress all players on grid regardless of +8.0 positive standing
            let rat = rt.npc_entities.get(&rat_ball_id).expect("Belt rat entity");
            assert!(
                rat.aggression_threat.contains_key(&pirate_friend_ship_id),
                "Pirate belt rats MUST aggress pilot despite +8.0 positive standing to pirate faction"
            );
        });
    }

    #[tokio::test]
    async fn test_hauler_convoy_multi_system_movement_and_cargo_drop() {
        // parity: feat:npc.npc-hauler
        eve_testkit::attest(&["feat:npc.npc-hauler"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);

        let convoy_id = 850_001i64;
        let hauler_type_id = 587u32; // Tayra / Industrial hauler
        let cargo_ore_type_id = 1230u32; // Veldspar
        let cargo_qty = 5_000i32;

        // Register a living-world hauler convoy with a multi-system route: Jita -> Perimeter -> Urlen
        let route = vec![SYS_JITA, SYS_PERIMETER, SYS_URLEN];
        ctx.living_world.register_convoy(LivingWorldConvoy {
            convoy_id,
            archetype: NpcArchetype::HaulerConvoy,
            ship_type_id: hauler_type_id,
            origin_system_id: SYS_JITA,
            current_system_id: SYS_JITA,
            destination_system_id: SYS_URLEN,
            destination_station_id: 60_003_766,
            cargo_type_id: cargo_ore_type_id,
            cargo_quantity: cargo_qty,
            lod_level: 2, // Spawned in space
            destroyed: false,
        });
        ctx.living_world.set_convoy_route(convoy_id, route.clone());

        // Materialize in Jita space
        let _ = ctx.living_world.materialize_system_lod2(SYS_JITA, &ctx.systems, &ctx.sde).await;

        // Step 1: Living-world cluster tick advances convoy along route: Jumps stargate from Jita into Perimeter
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let transit_jita = LivingWorldDirector::compute_hauler_transit_duration_secs(&ctx.sde, hauler_type_id, SYS_JITA, None, Some(SYS_PERIMETER), 60_003_766);
        ctx.advance_secs(transit_jita - 1);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert_eq!(
            ctx.living_world.get_convoy(convoy_id).unwrap().current_system_id,
            SYS_JITA,
            "Hauler must still be in Jita before full transit elapsed"
        );
        ctx.advance_secs(1);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert_eq!(
            ctx.living_world.get_convoy(convoy_id).unwrap().current_system_id,
            SYS_PERIMETER
        );

        // Verify hauler in Perimeter has valid CombatState HP (> 0 HP, e.g. 5,000 HP)
        let perim_handle = ctx.systems.get_or_spawn(SYS_PERIMETER);
        let hauler_combat = perim_handle
            .ballpark
            .read()
            .unwrap()
            .combat_states
            .get(&convoy_id)
            .cloned()
            .expect("Hauler must have combat state in new system");
        assert!(
            hauler_combat.max_shield_hp > 0.0 && hauler_combat.structure_hp > 0.0,
            "Hauler must have valid CombatState HP (found max_shield = {}, structure = {})",
            hauler_combat.max_shield_hp,
            hauler_combat.structure_hp
        );

        // Step 2: Living-world cluster tick advances from Perimeter into Urlen
        let transit_perim = LivingWorldDirector::compute_hauler_transit_duration_secs(&ctx.sde, hauler_type_id, SYS_PERIMETER, Some(SYS_JITA), Some(SYS_URLEN), 60_003_766);
        ctx.advance_secs(transit_perim - 1);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert_eq!(
            ctx.living_world.get_convoy(convoy_id).unwrap().current_system_id,
            SYS_PERIMETER,
            "Hauler must still be in Perimeter before full transit elapsed"
        );
        ctx.advance_secs(1);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert_eq!(
            ctx.living_world.get_convoy(convoy_id).unwrap().current_system_id,
            SYS_URLEN
        );

        // Step 3: Living-world cluster tick arrives at destination system, docks and despawns at destination station
        let transit_urlen = LivingWorldDirector::compute_hauler_transit_duration_secs(&ctx.sde, hauler_type_id, SYS_URLEN, Some(SYS_PERIMETER), None, 60_003_766);
        ctx.advance_secs(transit_urlen);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(ctx.living_world.is_convoy_docked(convoy_id));

        // 4. Test authentic cargo loot drop upon hauler destruction
        let hauler2_id = 850_002i64;
        let custom_cargo_tid = 1230u32; // Veldspar
        let custom_cargo_amount = 7_500i32;

        ctx.living_world.register_convoy(LivingWorldConvoy {
            convoy_id: hauler2_id,
            archetype: NpcArchetype::HaulerConvoy,
            ship_type_id: hauler_type_id,
            origin_system_id: SYS_JITA,
            current_system_id: SYS_JITA,
            destination_system_id: SYS_JITA,
            destination_station_id: 60_003_760,
            cargo_type_id: custom_cargo_tid,
            cargo_quantity: custom_cargo_amount,
            lod_level: 2,
            destroyed: false,
        });
        let _ = ctx.living_world.materialize_system_lod2(SYS_JITA, &ctx.systems, &ctx.sde).await;

        let killer_char_id = 930_001i64;
        let (wreck_id, _bounty, dropped_types) = destroy_specialized_npc_and_queue_bounty(
            &ctx,
            SYS_JITA,
            hauler2_id,
            killer_char_id,
        )
        .await;

        assert!(
            dropped_types.contains(&custom_cargo_tid),
            "Dropped loot must contain the hauler's defined cargo type ID"
        );

        // Verify loot item inside wreck cargo
        ctx.db.with_snapshot(|snap| {
            let cargo_item = snap
                .items
                .values()
                .find(|i| i.location_id == wreck_id && i.flag == InventoryFlag::Cargo && i.type_id == custom_cargo_tid)
                .expect("Wreck must contain dropped convoy cargo");
            assert_eq!(
                cargo_item.quantity,
                custom_cargo_amount,
                "Wreck cargo quantity must match hauler's defined quantity, not hardcoded defaults"
            );
        });

        // Convoy marked destroyed
        assert!(ctx.living_world.get_convoy(hauler2_id).unwrap().destroyed);
    }

    #[tokio::test]
    async fn test_encounter_site_destruction_trigger_and_wave_progression() {
        // parity: feat:npc.npc-spawn-encounter
        eve_testkit::attest(&["feat:npc.npc-spawn-encounter"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);

        let encounter_id = 77_001u32;
        let origin_pos = DVec3::new(100_000.0, 0.0, 0.0);

        // Wave 1: [2370, 2372] (Serpentis Initiate, Angel Rogue), Wave 2: [2379, 2381] (Serpentis Spy, Serpentis Chief Spy)
        let wave_defs = vec![vec![2370, 2372], vec![2379, 2381]];
        let wave1_balls = create_custom_encounter_site(
            &ctx,
            encounter_id,
            SYS_JITA,
            wave_defs,
            origin_pos,
        )
        .await;

        assert_eq!(wave1_balls.len(), 2);
        let trigger_ball_1 = wave1_balls[0];

        with_combat_runtime(&ctx, |rt| {
            let enc = rt.encounter_waves.get(&encounter_id).expect("Encounter director");
            assert_eq!(enc.current_wave, 1);
            assert_eq!(enc.trigger_ball_id, trigger_ball_1);
            assert!(!enc.completed);
        });

        // Deal lethal weapon damage in space to the designated trigger NPC
        let handle = ctx.systems.get_or_spawn(SYS_JITA);
        let lethal_damage = DamageVector {
            em: 50_000.0,
            thermal: 50_000.0,
            kinetic: 50_000.0,
            explosive: 50_000.0,
        };
        let dmg_res = handle.apply_damage(trigger_ball_1, lethal_damage).await;
        assert!(dmg_res.is_ok(), "apply_damage must succeed");

        // Hull reduced to 0; handle_npc_destroyed_in_space runs via npc_death_hook
        // Verify Wave 2 spawned with authentic types [2379, 2381]
        let (wave2_balls, trigger_ball_2) = with_combat_runtime(&ctx, |rt| {
            let enc = rt.encounter_waves.get(&encounter_id).expect("Encounter director");
            assert_eq!(enc.current_wave, 2, "Wave 2 must be triggered upon trigger NPC destruction");
            assert!(!enc.completed);
            (enc.active_wave_ball_ids.clone(), enc.trigger_ball_id)
        });

        assert!(!wave2_balls.is_empty(), "Wave 2 NPCs must be spawned in space");
        assert_ne!(trigger_ball_2, trigger_ball_1);

        // Verify wave 2 types in npc_entities
        let wave2_new_balls: Vec<i64> = wave2_balls
            .iter()
            .filter(|&&b| !wave1_balls.contains(&b))
            .copied()
            .collect();
        assert_eq!(wave2_new_balls.len(), 2, "Wave 2 must spawn 2 new NPC balls");

        with_combat_runtime(&ctx, |rt| {
            for &bid in &wave2_new_balls {
                let entity = rt.npc_entities.get(&bid).expect("Wave 2 entity");
                assert!(
                    entity.type_id == 2379 || entity.type_id == 2381,
                    "Wave 2 NPC must have authentic types from wave definition"
                );
                assert!(
                    entity.bounty_isk > 0.0,
                    "Wave 2 NPC must have authentic bounty ISK derived from SDE"
                );
            }
        });

        {
            let bp = handle.ballpark.read().unwrap();
            for &bid in &wave2_new_balls {
                let cs = bp.combat_states.get(&bid).expect("Wave 2 CombatState");
                assert!(
                    cs.max_structure_hp > 0.0 && cs.max_shield_hp > 0.0,
                    "Wave 2 NPC CombatState must have authentic HP derived from SDE"
                );
            }
        }

        // Eliminate all active NPCs with lethal damage (wave 1 remainder and wave 2)
        for &bid in &wave2_balls {
            let _ = handle.apply_damage(bid, lethal_damage).await;
        }

        // Site completed!
        with_combat_runtime(&ctx, |rt| {
            let enc = rt.encounter_waves.get(&encounter_id).expect("Encounter director");
            assert!(enc.completed, "Site must be marked completed when final wave is eliminated");
        });
    }
}
